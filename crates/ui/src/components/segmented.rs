use egui::{
    Align2, CornerRadius, CursorIcon, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
    pos2, vec2,
};

use crate::motion::{self, Spring};
use crate::theme::{self, control, font, radius, text};

pub struct Segmented<'a, T> {
    value: &'a mut T,
    options: &'a [(T, &'a str)],
}

impl<'a, T: PartialEq + Copy> Segmented<'a, T> {
    pub fn new(value: &'a mut T, options: &'a [(T, &'a str)]) -> Self {
        Self { value, options }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let pad = 2.0;
        let label_font = font::medium(text::SM + 1.0);
        let widest = self
            .options
            .iter()
            .map(|(_, label)| {
                ui.painter()
                    .layout_no_wrap(label.to_string(), label_font.clone(), p.text)
                    .size()
                    .x
            })
            .fold(0.0, f32::max);
        let segment = vec2(widest + 20.0, control::SM - pad * 2.0);
        let size = vec2(
            segment.x * self.options.len() as f32 + pad * 2.0,
            control::SM,
        );
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::RadioGroup, true, ""));

        let selected = self
            .options
            .iter()
            .position(|(v, _)| v == self.value)
            .unwrap_or(0);
        let mut target = selected;
        let mut hovered = None;
        for (i, (value, _)) in self.options.iter().enumerate() {
            let seg = Rect::from_min_size(
                pos2(rect.left() + pad + segment.x * i as f32, rect.top() + pad),
                segment,
            );
            let r = ui
                .interact(seg, response.id.with(i), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand);
            if r.hovered() {
                hovered = Some(i);
            }
            if r.clicked() && *self.value != *value {
                *self.value = *value;
                target = i;
                response.mark_changed();
            }
        }

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let corner = CornerRadius::same(radius::MD);
            painter.rect_filled(rect, corner, p.surface);
            painter.rect_stroke(rect, corner, Stroke::new(1.0, p.border), StrokeKind::Inside);

            // the highlight slides between segments instead of jumping, like a native segmented control
            let x = motion::spring(
                ui.ctx(),
                response.id.with("thumb"),
                target as f32,
                Spring::SNAPPY,
            );
            let thumb = Rect::from_min_size(
                pos2(rect.left() + pad + segment.x * x, rect.top() + pad),
                segment,
            );
            painter.rect_filled(thumb, CornerRadius::same(radius::MD - 1), p.active);
            painter.rect_stroke(
                thumb,
                CornerRadius::same(radius::MD - 1),
                Stroke::new(1.0, p.border_strong),
                StrokeKind::Inside,
            );

            for (i, (_, label)) in self.options.iter().enumerate() {
                let center = pos2(
                    rect.left() + pad + segment.x * (i as f32 + 0.5),
                    rect.center().y,
                );
                let color = if i == target || hovered == Some(i) {
                    p.text
                } else {
                    p.text_muted
                };
                painter.text(
                    center,
                    Align2::CENTER_CENTER,
                    label,
                    label_font.clone(),
                    color,
                );
            }
        }
        response
    }
}
