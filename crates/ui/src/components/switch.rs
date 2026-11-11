use egui::{CornerRadius, CursorIcon, Response, Sense, Ui, WidgetInfo, pos2, vec2};

use crate::motion::{Spring, spring_bool};
use crate::theme;

// an on and off switch whose knob slides across, for settings that are simply on or off
pub struct Switch<'a> {
    on: &'a mut bool,
    label: &'a str,
}

impl<'a> Switch<'a> {
    pub fn new(on: &'a mut bool) -> Self {
        Self { on, label: "" }
    }

    // what screen readers and test scripts call it
    pub fn label(mut self, label: &'a str) -> Self {
        self.label = label;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let (rect, mut response) = ui.allocate_exact_size(vec2(34.0, 20.0), Sense::click());
        let on = *self.on;
        response.widget_info(|| WidgetInfo::selected(egui::Role::Switch, true, on, self.label));
        if response.clicked() {
            *self.on = !*self.on;
            response.mark_changed();
        }
        if ui.is_rect_visible(rect) {
            let t = spring_bool(ui.ctx(), response.id, *self.on, Spring::SNAPPY);
            let off = theme::tint(p, 0.14 + 0.04 * super::hover_t(ui, &response));
            ui.painter()
                .rect_filled(rect, CornerRadius::same(10), off.lerp_to_gamma(p.accent, t));
            let x = rect.left() + 10.0 + (rect.width() - 20.0) * t;
            ui.painter()
                .circle_filled(pos2(x, rect.center().y), 7.0, egui::Color32::WHITE);
        }
        response.on_hover_cursor(CursorIcon::PointingHand)
    }
}
