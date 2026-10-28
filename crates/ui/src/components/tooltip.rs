use egui::{Align2, Id, KeyboardShortcut, Margin, Order, Response, Ui, pos2};

use super::Kbd;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, font, text};

const DELAY: f64 = 0.5;
// cards hold information you hovered on purpose to read, so they skip most of the wait
const CARD_DELAY: f64 = 0.12;
// moving between neighbouring controls shows the next tooltip right away, like macos
const GRACE: f64 = 0.4;
const GAP: f32 = 6.0;
// keeps tooltips off the window's rounded edge
const EDGE: f32 = 8.0;

pub trait TooltipExt {
    fn tip(self, text: impl Into<String>) -> Self;
    fn tip_with_shortcut(self, text: impl Into<String>, shortcut: KeyboardShortcut) -> Self;
    fn tip_ui(self, add: impl FnOnce(&mut Ui)) -> Self;
}

impl TooltipExt for Response {
    fn tip(self, text: impl Into<String>) -> Self {
        show(self, text.into(), None)
    }

    fn tip_with_shortcut(self, text: impl Into<String>, shortcut: KeyboardShortcut) -> Self {
        show(self, text.into(), Some(shortcut))
    }

    fn tip_ui(self, add: impl FnOnce(&mut Ui)) -> Self {
        show_ui(self, Margin::same(12), CARD_DELAY, add)
    }
}

#[derive(Clone, Copy, Default)]
struct HoverState {
    since: Option<f64>,
    suppressed: bool,
}

fn show(response: Response, label: String, shortcut: Option<KeyboardShortcut>) -> Response {
    show_ui(response, Margin::symmetric(8, 5), DELAY, |ui| {
        let p = theme::palette(ui);
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(&label)
                    .font(font::regular(text::SM))
                    .color(p.text),
            );
            if let Some(shortcut) = &shortcut {
                Kbd::shortcut(ui, shortcut);
            }
        });
    })
}

fn show_ui(response: Response, margin: Margin, delay: f64, add: impl FnOnce(&mut Ui)) -> Response {
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
            .get_or_insert(if instant { now - delay } else { now });
        let wait = since + delay - now;
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
    // last frame's size says whether a tall card still fits below, near the bottom of the window it goes above
    let height = ctx
        .memory(|m| m.area_rect(id.with("area")))
        .map_or(32.0, |r| r.height());
    let below = rect.bottom() + GAP + height + EDGE < ctx.content_rect().bottom();
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
        .constrain_to(ctx.content_rect().shrink(EDGE))
        .interactable(false)
        .show(&ctx, |ui| {
            ui.multiply_opacity(t.min(1.0));
            theme::floating(ui).inner_margin(margin).show(ui, add);
        });
    response
}
