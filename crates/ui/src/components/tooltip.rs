use egui::{
    Align2, CornerRadius, Frame, Id, KeyboardShortcut, Margin, Order, Response, Shadow, Stroke,
    pos2,
};

use super::Kbd;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, font, radius, text};

const DELAY: f64 = 0.5;
// moving between neighbouring controls shows the next tooltip right away, like macos
const GRACE: f64 = 0.4;
const GAP: f32 = 6.0;

pub trait TooltipExt {
    fn tip(self, text: impl Into<String>) -> Self;
    fn tip_with_shortcut(self, text: impl Into<String>, shortcut: KeyboardShortcut) -> Self;
}

impl TooltipExt for Response {
    fn tip(self, text: impl Into<String>) -> Self {
        show(self, text.into(), None)
    }

    fn tip_with_shortcut(self, text: impl Into<String>, shortcut: KeyboardShortcut) -> Self {
        show(self, text.into(), Some(shortcut))
    }
}

#[derive(Clone, Copy, Default)]
struct HoverState {
    since: Option<f64>,
    suppressed: bool,
}

fn show(response: Response, label: String, shortcut: Option<KeyboardShortcut>) -> Response {
    let ctx = response.ctx.clone();
    let id = response.id.with("tip");
    let last_shown_id = Id::unique("mux-tip-last-shown");
    let now = ctx.input(|i| i.time);
    let pressed = ctx.input(|i| i.pointer.any_down());
    let last_shown: f64 = ctx
        .data(|d| d.get_temp(last_shown_id))
        .unwrap_or(f64::NEG_INFINITY);
    let mut state: HoverState = ctx.data(|d| d.get_temp(id)).unwrap_or_default();

    let visible = if !response.hovered() {
        state = HoverState::default();
        false
    } else if pressed || state.suppressed {
        // a click hides the tooltip until the pointer leaves, so it never covers what you clicked
        state.suppressed = true;
        false
    } else {
        let instant = now - last_shown < GRACE;
        let since = *state
            .since
            .get_or_insert(if instant { now - DELAY } else { now });
        let wait = since + DELAY - now;
        if wait > 0.0 {
            ctx.request_repaint_after_secs(wait as f32);
        }
        wait <= 0.0
    };
    ctx.data_mut(|d| d.insert_temp(id, state));
    if visible {
        ctx.data_mut(|d| d.insert_temp(last_shown_id, now));
    }

    let t = spring_bool(&ctx, id.with("t"), visible, Spring::QUICK);
    if t <= 0.001 {
        return response;
    }

    let rect = response.rect;
    let below = rect.bottom() + GAP + 32.0 < ctx.content_rect().bottom();
    let slide = (1.0 - t) * 4.0;
    let (pivot, anchor) = if below {
        (
            Align2::CENTER_TOP,
            pos2(rect.center().x, rect.bottom() + GAP - slide),
        )
    } else {
        (
            Align2::CENTER_BOTTOM,
            pos2(rect.center().x, rect.top() - GAP + slide),
        )
    };

    egui::Area::new(id.with("area"))
        .order(Order::Tooltip)
        .pivot(pivot)
        .fixed_pos(anchor)
        .interactable(false)
        .show(&ctx, |ui| {
            let p = theme::palette(ui);
            ui.multiply_opacity(t.min(1.0));
            Frame::new()
                .fill(p.elevated)
                .stroke(Stroke::new(1.0, p.border_strong))
                .corner_radius(CornerRadius::same(radius::MD))
                .shadow(Shadow {
                    offset: [0, 4],
                    blur: 12,
                    spread: 0,
                    color: ui.visuals().popup_shadow.color,
                })
                .inner_margin(Margin::symmetric(8, 5))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(&label)
                                .font(font::regular(text::SM + 1.0))
                                .color(p.text),
                        );
                        if let Some(shortcut) = &shortcut {
                            Kbd::shortcut(ui, shortcut);
                        }
                    });
                });
        });
    response
}
