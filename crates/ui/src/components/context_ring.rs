use egui::{Align, CornerRadius, CursorIcon, Layout, Response, RichText, Sense, Ui, Vec2};
use mux_core::protocol::ContextUsage;

use super::usage_ring::{BAR, WIDTH, bar, level_color, ring};
use super::{TooltipExt, hover_t};
use crate::theme::{self, control, font, radius, space, text};

// a small ring for how much of the model's context window the conversation fills
pub struct ContextRing {
    context: ContextUsage,
}

impl ContextRing {
    pub fn new(context: ContextUsage) -> Self {
        Self { context }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let used = share(self.context);
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(control::SM), Sense::hover());
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
        response
            .on_hover_cursor(CursorIcon::Default)
            .tip_ui(|ui| details(ui, context))
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
fn tokens(count: u64) -> String {
    let (value, unit) = match count {
        0..1_000 => return count.to_string(),
        1_000..1_000_000 => (count as f64 / 1_000.0, "k"),
        _ => (count as f64 / 1_000_000.0, "M"),
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
    }
}
