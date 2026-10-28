use egui::{Align2, Color32, Response, Sense, Ui, Vec2};

use crate::theme::{self, font};

pub struct Icon {
    glyph: char,
    size: f32,
    color: Option<Color32>,
    fill: bool,
}

impl Icon {
    pub fn new(glyph: char) -> Self {
        Self {
            glyph,
            size: 16.0,
            color: None,
            fill: false,
        }
    }

    pub fn size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Color32) -> Self {
        self.color = Some(color);
        self
    }

    pub fn fill(mut self, fill: bool) -> Self {
        self.fill = fill;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(self.size), Sense::hover());
        if ui.is_rect_visible(rect) {
            let color = self.color.unwrap_or(theme::palette(ui).text);
            let font = if self.fill {
                font::icon_fill(self.size)
            } else {
                font::icon(self.size)
            };
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                self.glyph,
                font,
                color,
            );
        }
        response
    }
}
