use egui::{Align2, CornerRadius, CursorIcon, Response, Sense, Ui, Vec2, WidgetInfo};

use super::{Size, hover_t, press_t};
use crate::theme::{self, control, font, radius};

pub struct IconButton {
    glyph: char,
    label: &'static str,
    size: Size,
    selected: bool,
}

impl IconButton {
    pub fn new(glyph: char, label: &'static str) -> Self {
        Self {
            glyph,
            label,
            size: Size::Sm,
            selected: false,
        }
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let side = match self.size {
            Size::Sm => control::SM,
            Size::Md => control::MD,
        };
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, self.label));

        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let pressed = press_t(ui, &response);
            let base = if self.selected {
                p.active
            } else {
                p.hover.gamma_multiply(0.0)
            };
            let fill = base
                .lerp_to_gamma(p.hover, t)
                .gamma_multiply(1.0 - 0.15 * pressed);
            let fg = if self.selected {
                p.text
            } else {
                p.text_muted.lerp_to_gamma(p.text, t)
            };
            let rect = rect.shrink(side * 0.03 * pressed);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(radius::MD), fill);
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                self.glyph,
                font::icon(16.0),
                fg,
            );
        }

        response.on_hover_cursor(CursorIcon::PointingHand)
    }
}
