use egui::{
    Align, Align2, CornerRadius, CursorIcon, Layout, Order, Rect, RichText, Sense, Ui, Vec2,
};
use mux_core::protocol::ContextUsage;

use super::usage_ring::{BAR, WIDTH, bar, level_color, ring};
use super::{Button, Size, hover_t};
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, control, font, radius, space, text};

// how long the pointer rests on the ring before the card opens, and how long it lingers once left
const OPEN_AFTER: f64 = 0.12;
const CLOSE_AFTER: f64 = 0.2;

// a small ring for how much of the model's context window the conversation fills
pub struct ContextRing {
    context: ContextUsage,
    can_compact: bool,
}

impl ContextRing {
    pub fn new(context: ContextUsage) -> Self {
        Self {
            context,
            can_compact: true,
        }
    }

    // compacting waits for the agent to finish what it's doing
    pub fn can_compact(mut self, can_compact: bool) -> Self {
        self.can_compact = can_compact;
        self
    }

    // clicking the ring offers to compact, and this says whether that was picked
    pub fn show(self, ui: &mut Ui) -> bool {
        let p = theme::palette(ui);
        let used = share(self.context);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(control::SM), Sense::click());
        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(radius::MD),
                p.hover.gamma_multiply(t),
            );
            ring(ui, rect.center(), used);
        }
        let context = self.context;
        let response = response.on_hover_cursor(CursorIcon::PointingHand);

        // a hover card rather than a tooltip, so the pointer can move onto it and press compact
        let id = response.id.with("card");
        let card: Option<Rect> = ui.data(|d| d.get_temp(id.with("rect")));
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let now = ui.input(|i| i.time);
        // the gap between the ring and the card counts as on the card, so crossing it doesn't close it
        let over_card = card.is_some_and(|card| {
            pointer.is_some_and(|pos| card.union(rect).expand(space::XS).contains(pos))
        });
        // stored as options, and read back as the same type, since egui keys its memory by type too
        let mut since: Option<f64> = ui
            .data(|d| d.get_temp::<Option<f64>>(id.with("since")))
            .flatten();
        let mut left: Option<f64> = ui
            .data(|d| d.get_temp::<Option<f64>>(id.with("left")))
            .flatten();
        let was_open: bool = ui.data(|d| d.get_temp(id.with("open"))).unwrap_or(false);
        let mut open = was_open;
        if response.clicked() {
            open = true;
        }
        if response.hovered() {
            let start = *since.get_or_insert(now);
            if now - start >= OPEN_AFTER {
                open = true;
            } else {
                ui.ctx()
                    .request_repaint_after_secs((start + OPEN_AFTER - now) as f32);
            }
        } else {
            since = None;
        }
        if open && (response.hovered() || over_card) {
            left = None;
        } else if open {
            let start = *left.get_or_insert(now);
            if now - start >= CLOSE_AFTER {
                open = false;
                left = None;
            } else {
                ui.ctx()
                    .request_repaint_after_secs((start + CLOSE_AFTER - now) as f32);
            }
        }
        ui.data_mut(|d| {
            d.insert_temp(id.with("since"), since);
            d.insert_temp(id.with("left"), left);
            d.insert_temp(id.with("open"), open);
        });

        let mut compact = false;
        let t = spring_bool(ui.ctx(), id.with("t"), open, Spring::QUICK);
        if t > 0.01 {
            let can_compact = self.can_compact;
            let shown = egui::Area::new(id)
                .order(Order::Foreground)
                .pivot(Align2::RIGHT_BOTTOM)
                .fixed_pos(rect.right_top() - Vec2::new(0.0, space::XS + (1.0 - t) * 4.0))
                .constrain_to(ui.ctx().content_rect().shrink(space::SM))
                .show(ui.ctx(), |ui| {
                    ui.multiply_opacity(t);
                    theme::floating(ui)
                        .inner_margin(egui::Margin::same(space::MD as i8))
                        .show(ui, |ui| {
                            details(ui, context);
                            ui.add_space(space::MD);
                            if can_compact {
                                compact =
                                    Button::new("Compact now").size(Size::Sm).show(ui).clicked();
                            } else {
                                // a disabled button reads as broken, so a working agent just says when it'll be possible
                                let p = theme::palette(ui);
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = space::XS;
                                    ui.label(
                                        RichText::new(crate::icons::CLOCK.to_string())
                                            .font(font::icon(crate::theme::icon::SM))
                                            .color(p.text_faint),
                                    );
                                    ui.label(
                                        RichText::new("You can compact once it finishes")
                                            .font(font::regular(text::SM))
                                            .color(p.text_faint),
                                    );
                                });
                            }
                        });
                });
            ui.data_mut(|d| d.insert_temp(id.with("rect"), shown.response.rect));
        }
        if compact {
            ui.data_mut(|d| d.insert_temp(id.with("open"), false));
        }
        compact
    }
}

fn share(context: ContextUsage) -> f32 {
    if context.window == 0 {
        0.0
    } else {
        (context.used as f32 / context.window as f32).clamp(0.0, 1.0)
    }
}

fn details(ui: &mut Ui, context: ContextUsage) {
    let p = theme::palette(ui);
    let used = share(context);
    ui.set_width(WIDTH);
    ui.spacing_mut().item_spacing = Vec2::ZERO;
    ui.spacing_mut().interact_size.y = 0.0;
    ui.label(
        RichText::new("Context window")
            .font(font::medium(text::SM))
            .color(p.text_muted),
    );
    ui.add_space(space::MD);
    ui.horizontal(|ui| {
        let amount = if context.window == 0 {
            format!("{} tokens", tokens(context.used))
        } else {
            format!(
                "{} of {} tokens",
                tokens(context.used),
                tokens(context.window)
            )
        };
        ui.label(
            RichText::new(amount)
                .font(font::medium(text::MD))
                .color(p.text),
        );
        if context.window > 0 {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{}%", (used * 100.0).round() as u32))
                        .font(font::medium(text::SM))
                        .color(level_color(p, used)),
                );
            });
        }
    });
    ui.add_space(space::SM);
    let (row, _) = ui.allocate_exact_size(Vec2::new(WIDTH, BAR), Sense::hover());
    bar(ui, row, used);
    ui.add_space(space::SM);
    ui.add(
        egui::Label::new(
            RichText::new("Older messages are summarised as it fills")
                .font(font::regular(text::SM))
                .color(p.text_faint),
        )
        .wrap(),
    );
}

// 23612 reads as 23.6k and 1000000 as 1M
pub fn tokens(count: u64) -> String {
    let (value, unit) = match count {
        0..1_000 => return count.to_string(),
        1_000..1_000_000 => (count as f64 / 1_000.0, "k"),
        1_000_000..1_000_000_000 => (count as f64 / 1_000_000.0, "M"),
        _ => (count as f64 / 1_000_000_000.0, "B"),
    };
    let rounded = format!("{value:.1}");
    format!("{}{unit}", rounded.trim_end_matches(".0"))
}

#[cfg(test)]
mod tests {
    use super::tokens;

    #[test]
    fn shortens_token_counts() {
        assert_eq!(tokens(812), "812");
        assert_eq!(tokens(23_612), "23.6k");
        assert_eq!(tokens(272_000), "272k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_615_335_911), "1.6B");
    }
}
