use std::f32::consts::FRAC_PI_2;

use egui::epaint::TextShape;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Rect, Response, Sense, Ui, Vec2, WidgetInfo, pos2,
};

use super::hover_t;
use crate::icons;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, control, font, icon, radius, space, text};

const HEIGHT: f32 = 36.0;

pub struct GroupHeader<'a> {
    label: &'a str,
    detail: &'a str,
    open: bool,
    drop_target: bool,
}

pub struct GroupHeaderResponse {
    pub response: Response,
    // the + on the right, for starting something new in this group
    pub add: Response,
}

impl<'a> GroupHeader<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            detail: "",
            open: true,
            drop_target: false,
        }
    }

    // faint text after the name, like the folder the group points at
    pub fn detail(mut self, detail: &'a str) -> Self {
        self.detail = detail;
        self
    }

    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    pub fn drop_target(mut self, drop_target: bool) -> Self {
        self.drop_target = drop_target;
        self
    }

    pub fn show(self, ui: &mut Ui) -> GroupHeaderResponse {
        let p = theme::palette(ui);
        let size = Vec2::new(ui.available_width(), HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, self.label));

        let button = Vec2::splat(control::SM);
        let add_rect = Rect::from_center_size(
            pos2(rect.right() - space::XS - button.x / 2.0, rect.center().y),
            button,
        );
        let caret_center = pos2(add_rect.left() - button.x / 2.0, rect.center().y);
        // registered after the header so it sits on top and takes its own clicks
        let add = ui
            .interact(add_rect, response.id.with("add"), Sense::click())
            .on_hover_cursor(CursorIcon::PointingHand);

        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let target = spring_bool(
                ui.ctx(),
                response.id.with("target"),
                self.drop_target && response.contains_pointer(),
                Spring::QUICK,
            );
            let fill = Color32::TRANSPARENT
                .lerp_to_gamma(theme::tint(p, theme::fill::HOVER), t)
                .lerp_to_gamma(p.accent.gamma_multiply(0.16), target);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(radius::LG), fill);

            let folder_x = rect.left() + space::SM + icon::LG / 2.0;
            ui.painter().text(
                pos2(folder_x, rect.center().y),
                Align2::CENTER_CENTER,
                if self.open {
                    icons::FOLDER_OPEN
                } else {
                    icons::FOLDER
                },
                font::icon(icon::LG),
                p.text_muted,
            );
            let name = ui.painter().text(
                pos2(folder_x + icon::LG / 2.0 + space::SM, rect.center().y),
                Align2::LEFT_CENTER,
                self.label,
                font::semibold(text::MD + 1.0),
                p.text,
            );
            if !self.detail.is_empty() {
                let room = Rect::from_min_max(
                    pos2(name.right() + space::SM, rect.top()),
                    pos2(caret_center.x - button.x / 2.0, rect.bottom()),
                );
                ui.painter().with_clip_rect(room).text(
                    pos2(room.left(), rect.center().y),
                    Align2::LEFT_CENTER,
                    self.detail,
                    font::regular(text::SM + 1.0),
                    p.text_faint,
                );
            }

            let turn = spring_bool(
                ui.ctx(),
                response.id.with("open"),
                self.open,
                Spring::SMOOTH,
            );
            let caret = ui.painter().layout_no_wrap(
                icons::CARET_RIGHT.to_string(),
                font::icon(icon::SM),
                p.text_faint,
            );
            ui.painter().add(
                TextShape::new(caret_center - caret.size() / 2.0, caret, p.text_faint)
                    .with_angle_and_anchor(turn * FRAC_PI_2, Align2::CENTER_CENTER),
            );

            let add_t = hover_t(ui, &add);
            ui.painter().rect_filled(
                add_rect,
                CornerRadius::same(radius::MD),
                // stronger than the row's own hover, since it sits on top of it
                theme::tint(p, theme::fill::SELECTED * add_t),
            );
            ui.painter().text(
                add_rect.center(),
                Align2::CENTER_CENTER,
                icons::PLUS,
                font::icon(icon::MD),
                p.text_muted.lerp_to_gamma(p.text, add_t),
            );
        }

        GroupHeaderResponse {
            response: response.on_hover_cursor(CursorIcon::PointingHand),
            add,
        }
    }
}
