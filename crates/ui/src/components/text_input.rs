use egui::{CornerRadius, Frame, Margin, Response, Stroke, TextEdit, Ui, Vec2};

use crate::theme::{self, control, radius};

pub struct TextInput<'a> {
    value: &'a mut String,
    placeholder: &'a str,
    width: Option<f32>,
}

impl<'a> TextInput<'a> {
    pub fn new(value: &'a mut String) -> Self {
        Self {
            value,
            placeholder: "",
            width: None,
        }
    }

    pub fn placeholder(mut self, placeholder: &'a str) -> Self {
        self.placeholder = placeholder;
        self
    }

    pub fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let width = self.width.unwrap_or_else(|| ui.available_width());
        let id = ui.next_auto_id();
        let focused = ui.memory(|m| m.has_focus(id));
        let border = if focused { p.accent } else { p.border_strong };
        let response = TextEdit::singleline(self.value)
            .id(id)
            .hint_text(self.placeholder)
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, border))
                    .corner_radius(CornerRadius::same(radius::MD))
                    .inner_margin(Margin::symmetric(10, 6)),
            )
            .min_size(Vec2::new(width, control::MD))
            .desired_width(width - 20.0)
            .show(ui)
            .response
            .response;
        super::edit_menu(&response, self.value);
        response
    }
}
