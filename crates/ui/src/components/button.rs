use egui::{
    Align2, CornerRadius, CursorIcon, Response, Sense, Stroke, StrokeKind, TextStyle, Ui, Vec2,
    WidgetInfo,
};

use super::{hover_t, press_t};
use crate::icons;
use crate::theme::{self, control, font, icon, radius, space};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Sm,
    Md,
}

pub struct Button {
    text: String,
    variant: Variant,
    size: Size,
    enabled: bool,
    // a down arrow after the text, for a button that opens a list to pick from
    menu: bool,
}

impl Button {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            variant: Variant::Secondary,
            size: Size::Md,
            enabled: true,
            menu: false,
        }
    }

    // marks the button as opening a menu, the way the composer's pickers show it
    pub fn menu(mut self) -> Self {
        self.menu = true;
        self
    }

    pub fn variant(mut self, variant: Variant) -> Self {
        self.variant = variant;
        self
    }

    pub fn primary(self) -> Self {
        self.variant(Variant::Primary)
    }

    pub fn ghost(self) -> Self {
        self.variant(Variant::Ghost)
    }

    pub fn danger(self) -> Self {
        self.variant(Variant::Danger)
    }

    pub fn size(mut self, size: Size) -> Self {
        self.size = size;
        self
    }

    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let font = TextStyle::Button.resolve(ui.style());
        let galley = ui.painter().layout_no_wrap(self.text.clone(), font, p.text);
        let (height, pad) = match self.size {
            Size::Sm => (control::SM, 8.0),
            Size::Md => (control::MD, 12.0),
        };
        let caret = if self.menu { icon::SM + space::XS } else { 0.0 };
        let size = Vec2::new(galley.size().x + caret + pad * 2.0, height);
        let sense = if self.enabled {
            Sense::click()
        } else {
            Sense::hover()
        };
        let (rect, response) = ui.allocate_exact_size(size, sense);
        response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, self.enabled, &self.text));

        if ui.is_rect_visible(rect) {
            let t = hover_t(ui, &response);
            let pressed = press_t(ui, &response);
            let (base, hover, fg, stroke) = match self.variant {
                Variant::Primary => (p.accent, p.accent_hover, p.on_accent, None),
                Variant::Secondary => (p.surface, p.hover, p.text, Some(p.border_strong)),
                Variant::Ghost => (
                    p.bg.gamma_multiply(0.0),
                    p.hover,
                    p.text_muted.lerp_to_gamma(p.text, t),
                    None,
                ),
                Variant::Danger => (
                    p.danger.gamma_multiply(0.14),
                    p.danger.gamma_multiply(0.24),
                    p.danger,
                    None,
                ),
            };
            let fill = base
                .lerp_to_gamma(hover, t)
                .gamma_multiply(1.0 - 0.15 * pressed);
            // shrink slightly while held like apple's press feedback
            let rect = rect.shrink2(rect.size() * 0.015 * pressed);
            let alpha = if self.enabled { 1.0 } else { 0.4 };
            let corner = CornerRadius::same(radius::MD);
            let painter = ui.painter();
            painter.rect_filled(rect, corner, fill.gamma_multiply(alpha));
            if let Some(stroke) = stroke {
                painter.rect_stroke(
                    rect,
                    corner,
                    Stroke::new(1.0, stroke.gamma_multiply(alpha)),
                    StrokeKind::Inside,
                );
            }
            let text_at = Align2::CENTER_CENTER
                .anchor_size(rect.center() - Vec2::new(caret / 2.0, 0.0), galley.size())
                .min;
            let text_right = text_at.x + galley.size().x;
            painter.galley_with_override_text_color(text_at, galley, fg.gamma_multiply(alpha));
            if self.menu {
                painter.text(
                    egui::pos2(text_right + space::XS + icon::SM / 2.0, rect.center().y),
                    Align2::CENTER_CENTER,
                    icons::CARET_DOWN,
                    font::icon(icon::SM),
                    p.text_faint.gamma_multiply(alpha),
                );
            }
        }

        if self.enabled {
            response.on_hover_cursor(CursorIcon::PointingHand)
        } else {
            response
        }
    }
}
