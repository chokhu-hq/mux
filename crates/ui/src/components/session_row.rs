use egui::{Align2, CornerRadius, CursorIcon, Rect, Response, Sense, Ui, Vec2, WidgetInfo, pos2};
use mux_core::{SessionKind, SessionStatus};

use super::{Badge, StatusDot, hover_t};
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, font, radius, space, text};

pub struct SessionRow<'a> {
    title: &'a str,
    kind: SessionKind,
    status: SessionStatus,
    meta: &'a str,
    selected: bool,
}

impl<'a> SessionRow<'a> {
    pub const HEIGHT: f32 = 44.0;

    pub fn new(title: &'a str, kind: SessionKind, status: SessionStatus) -> Self {
        Self {
            title,
            kind,
            status,
            meta: "",
            selected: false,
        }
    }

    pub fn meta(mut self, meta: &'a str) -> Self {
        self.meta = meta;
        self
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let size = Vec2::new(ui.available_width(), Self::HEIGHT);
        let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, self.title));

        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let selected = spring_bool(
                ui.ctx(),
                response.id.with("selected"),
                self.selected,
                Spring::SMOOTH,
            );
            let fill =
                p.bg.gamma_multiply(0.0)
                    .lerp_to_gamma(p.hover, t)
                    .lerp_to_gamma(p.active, selected);
            ui.painter()
                .rect_filled(rect, CornerRadius::same(radius::MD), fill);

            let inner = rect.shrink2(Vec2::new(space::SM + 2.0, space::XS + 2.0));
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(
                pos2(inner.min.x, inner.center().y - 6.0),
                Vec2::splat(12.0),
            )));
            StatusDot::new(self.status).show(&mut child);

            let text_x = inner.min.x + 12.0 + space::SM;
            let badge_w = 56.0;
            let max_title = inner.max.x - text_x - badge_w;
            let title = ui.painter().layout(
                self.title.to_owned(),
                font::medium(text::MD),
                p.text,
                f32::INFINITY,
            );
            let title_pos = pos2(text_x, inner.min.y);
            ui.painter()
                .with_clip_rect(Rect::from_min_size(title_pos, Vec2::new(max_title, 18.0)))
                .galley(title_pos, title, p.text);
            ui.painter().text(
                pos2(text_x, inner.max.y),
                Align2::LEFT_BOTTOM,
                self.meta,
                font::regular(text::SM),
                p.text_faint,
            );

            let mut child = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(
                        pos2(inner.max.x - badge_w, inner.min.y),
                        pos2(inner.max.x, inner.min.y + 18.0),
                    ))
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            Badge::kind(self.kind).show(&mut child);
        }

        response.on_hover_cursor(CursorIcon::PointingHand)
    }
}
