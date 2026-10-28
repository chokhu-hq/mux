use egui::{Align2, CornerRadius, CursorIcon, Response, Sense, Ui, Vec2, WidgetInfo, pos2};

use super::hover_t;
use crate::icons;
use crate::theme::{self, control, font, icon, radius, space, text};

// a compact label with a caret, used to open a menu of choices like a model picker
pub struct Chip<'a> {
    label: &'a str,
}

impl<'a> Chip<'a> {
    pub fn new(label: &'a str) -> Self {
        Self { label }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let galley = ui.painter().layout_no_wrap(
            self.label.to_owned(),
            font::medium(text::SM + 1.0),
            p.text_muted,
        );
        let width = space::SM + galley.size().x + space::XS + icon::SM + space::SM;
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(width, control::SM), Sense::click());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::ComboBox, true, self.label));

        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let open =
                egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
            let fill = p.hover.gamma_multiply(if open { 1.0 } else { t });
            ui.painter()
                .rect_filled(rect, CornerRadius::same(radius::MD), fill);
            let color = p
                .text_muted
                .lerp_to_gamma(p.text, if open { 1.0 } else { t });
            let x = rect.left() + space::SM;
            ui.painter().galley_with_override_text_color(
                pos2(x, rect.center().y - galley.size().y / 2.0),
                galley.clone(),
                color,
            );
            ui.painter().text(
                pos2(rect.right() - space::SM, rect.center().y),
                Align2::RIGHT_CENTER,
                icons::CARET_DOWN,
                font::icon(icon::SM),
                color,
            );
        }
        response.on_hover_cursor(CursorIcon::PointingHand)
    }
}
