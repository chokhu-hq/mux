use egui::{Align2, CornerRadius, CursorIcon, Response, Sense, Ui, Vec2, WidgetInfo, pos2};

use mux_core::Provider;

use super::{hover_t, provider_color, provider_mark};
use crate::icons;
use crate::theme::{self, control, font, icon, radius, space, text};

// a compact label with a caret, used to open a menu of choices like a model picker
pub struct Chip<'a> {
    label: &'a str,
    provider: Option<Provider>,
    icon: Option<char>,
}

impl<'a> Chip<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            label,
            provider: None,
            icon: None,
        }
    }

    // leads with an agent's logo, for pickers that choose between agents
    pub fn provider(mut self, provider: Provider) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn icon(mut self, icon: char) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let galley = ui.painter().layout_no_wrap(
            self.label.to_owned(),
            font::medium(text::SM + 1.0),
            p.text_muted,
        );
        let mark_w = if self.provider.is_some() || self.icon.is_some() {
            icon::MD + space::XS + 2.0
        } else {
            0.0
        };
        let label_w = if self.label.is_empty() {
            -space::XS
        } else {
            galley.size().x
        };
        let width = space::SM + mark_w + label_w + space::XS + icon::SM + space::SM;
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
            let mut x = rect.left() + space::SM;
            if let Some(provider) = self.provider {
                let center = pos2(x + icon::MD / 2.0, rect.center().y);
                provider_mark(
                    ui.painter(),
                    center,
                    icon::MD,
                    provider,
                    provider_color(provider, color),
                );
                x += mark_w;
            } else if let Some(glyph) = self.icon {
                ui.painter().text(
                    pos2(x + icon::MD / 2.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    glyph,
                    font::icon(icon::MD),
                    color,
                );
                x += mark_w;
            }
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
