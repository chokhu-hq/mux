use egui::{Align2, CornerRadius, Response, Sense, Ui, Vec2, WidgetInfo, pos2};

use super::hover_t;
use crate::theme::{self, control, font, icon, radius, space, text};

const MIN_WIDTH: f32 = 180.0;

pub struct MenuItem<'a> {
    icon: Option<char>,
    label: &'a str,
    danger: bool,
}

impl<'a> MenuItem<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            icon: None,
            label,
            danger: false,
        }
    }

    pub fn icon(mut self, icon: char) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let icon_w = if self.icon.is_some() {
            icon::MD + space::SM
        } else {
            0.0
        };
        let label_w = ui
            .painter()
            .layout_no_wrap(self.label.to_owned(), font::regular(text::MD), p.text)
            .size()
            .x;
        let natural = MIN_WIDTH.max(space::SM * 3.0 + icon_w + label_w);
        // menus measure on a sizing pass first, then every row fills the settled width
        let width = if ui.is_sizing_pass() {
            natural
        } else {
            ui.available_width().max(natural)
        };
        let size = Vec2::new(width, control::MD);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::MenuItem, true, self.label));

        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let (fg, hover) = if self.danger {
                (p.danger, p.danger.gamma_multiply(0.14))
            } else {
                (p.text, p.hover)
            };
            ui.painter().rect_filled(
                rect,
                CornerRadius::same(radius::MD),
                hover.gamma_multiply(t),
            );
            let mut x = rect.left() + space::SM;
            if let Some(icon) = self.icon {
                ui.painter().text(
                    pos2(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    icon,
                    font::icon(icon::MD),
                    if self.danger { fg } else { p.text_muted },
                );
                x += icon::MD + space::SM;
            }
            ui.painter().text(
                pos2(x, rect.center().y),
                Align2::LEFT_CENTER,
                self.label,
                font::regular(text::MD),
                fg,
            );
        }

        response
    }
}
