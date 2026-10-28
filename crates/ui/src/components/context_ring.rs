use egui::{Align, CornerRadius, CursorIcon, Layout, RichText, Sense, Ui, Vec2};
use mux_core::protocol::ContextUsage;

use super::usage_ring::{BAR, WIDTH, bar, level_color, ring};
use super::{MenuItem, TooltipExt, hover_t};
use crate::icons;
use crate::theme::{self, control, font, radius, space, text};

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
        let menu_open =
            egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
        let response = response.on_hover_cursor(CursorIcon::PointingHand);
        let response = if menu_open {
            response
        } else {
            response.tip_ui(|ui| details(ui, context))
        };
        let mut compact = false;
        egui::Popup::menu(&response)
            .align(egui::RectAlign::TOP_END)
            .gap(space::XS)
            .show(|ui| {
                ui.add_enabled_ui(self.can_compact, |ui| {
                    let detail = if self.can_compact {
                        "Summarise older messages to free up space"
                    } else {
                        "Available once the agent is done"
                    };
                    compact = MenuItem::new("Compact now")
                        .icon(icons::ARROWS_IN_LINE_VERTICAL)
                        .detail(detail)
                        .show(ui)
                        .clicked();
                });
            });
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
            RichText::new("Older messages are summarised as it fills. Click to compact now.")
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
