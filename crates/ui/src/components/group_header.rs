use std::f32::consts::FRAC_PI_2;

use egui::{
    Align2, CornerRadius, CursorIcon, Response, Sense, Ui, Vec2, WidgetInfo, epaint::TextShape,
    pos2,
};

use super::hover_t;
use crate::icons;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, control, font, icon, radius, space, text};

pub struct GroupHeader<'a> {
    label: &'a str,
    count: usize,
    open: bool,
    drop_target: bool,
}

impl<'a> GroupHeader<'a> {
    pub fn new(label: &'a str, count: usize) -> Self {
        Self {
            label,
            count,
            open: true,
            drop_target: false,
        }
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn drop_target(mut self, drop_target: bool) -> Self {
        self.drop_target = drop_target;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let size = Vec2::new(ui.available_width(), control::MD);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, self.label));

        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let target = spring_bool(
                ui.ctx(),
                response.id.with("target"),
                self.drop_target && response.contains_pointer(),
                Spring::QUICK,
            );
            let open = spring_bool(
                ui.ctx(),
                response.id.with("open"),
                self.open,
                Spring::SMOOTH,
            );
            let fill = p
                .hover
                .gamma_multiply(t)
                .lerp_to_gamma(p.accent.gamma_multiply(0.16), target);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(radius::MD), fill);

            let caret = ui.painter().layout_no_wrap(
                icons::CARET_RIGHT.to_string(),
                font::icon(icon::SM),
                p.text_faint,
            );
            let center = pos2(rect.left() + space::SM + 6.0, rect.center().y);
            ui.painter().add(
                TextShape::new(center - caret.size() / 2.0, caret, p.text_faint)
                    .with_angle_and_anchor(open * FRAC_PI_2, Align2::CENTER_CENTER),
            );

            let label_x = center.x + 6.0 + space::SM;
            let label = ui.painter().text(
                pos2(label_x, rect.center().y),
                Align2::LEFT_CENTER,
                self.label,
                font::medium(text::SM),
                p.text_muted.lerp_to_gamma(p.text, t),
            );
            ui.painter().text(
                pos2(label.right() + space::SM, rect.center().y),
                Align2::LEFT_CENTER,
                self.count.to_string(),
                font::regular(text::SM),
                p.text_faint,
            );
        }

        response.on_hover_cursor(CursorIcon::PointingHand)
    }
}
