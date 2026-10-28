use egui::{Align2, CornerRadius, KeyboardShortcut, Response, Sense, Stroke, StrokeKind, Ui, Vec2};

use crate::shortcut;
use crate::theme::{self, font, radius, text};

pub struct Kbd {
    keys: String,
}

impl Kbd {
    pub fn new(keys: impl Into<String>) -> Self {
        Self { keys: keys.into() }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let galley = ui
            .painter()
            .layout_no_wrap(self.keys, font::medium(text::SM), p.text_muted);
        let size = Vec2::new((galley.size().x + 10.0).max(20.0), 20.0);
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
        if ui.is_rect_visible(rect) {
            let corner = CornerRadius::same(radius::SM);
            ui.painter().rect_filled(rect, corner, p.surface);
            ui.painter().rect_stroke(
                rect,
                corner,
                Stroke::new(1.0, p.border_strong),
                StrokeKind::Inside,
            );
            ui.painter().galley(
                Align2::CENTER_CENTER
                    .anchor_size(rect.center(), galley.size())
                    .min,
                galley,
                p.text_muted,
            );
        }
        response
    }

    pub fn shortcut(ui: &mut Ui, shortcut: &KeyboardShortcut) {
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.x = 3.0;
            for key in shortcut::keys(shortcut) {
                Kbd::new(key).show(ui);
            }
        });
    }
}
