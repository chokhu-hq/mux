use egui::{
    Align2, CornerRadius, CursorIcon, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
    pos2, vec2,
};

use crate::motion::{self, Spring};
use crate::theme::{self, control, font, radius, space, text};

pub struct Segmented<'a, T> {
    value: &'a mut T,
    options: &'a [(T, &'a str)],
    fit: bool,
}

impl<'a, T: PartialEq + Copy> Segmented<'a, T> {
    pub fn new(value: &'a mut T, options: &'a [(T, &'a str)]) -> Self {
        Self {
            value,
            options,
            fit: false,
        }
    }

    // each segment as wide as its own label, for labels of very different lengths
    pub fn fit(mut self) -> Self {
        self.fit = true;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let pad = 2.0;
        let label_font = font::medium(text::MD);
        let labels: Vec<f32> = self
            .options
            .iter()
            .map(|(_, label)| {
                ui.painter()
                    .layout_no_wrap(label.to_string(), label_font.clone(), p.text)
                    .size()
                    .x
            })
            .collect();
        let widest = labels.iter().copied().fold(0.0, f32::max);
        let widths: Vec<f32> = labels
            .iter()
            .map(|w| {
                if self.fit {
                    w + space::MD * 2.0
                } else {
                    widest + 20.0
                }
            })
            .collect();
        let lefts: Vec<f32> = widths
            .iter()
            .scan(pad, |x, w| {
                let left = *x;
                *x += w;
                Some(left)
            })
            .collect();
        let height = control::SM - pad * 2.0;
        let size = vec2(widths.iter().sum::<f32>() + pad * 2.0, control::SM);
        let (rect, mut response) = ui.allocate_exact_size(size, Sense::hover());
        response.widget_info(|| WidgetInfo::labeled(egui::Role::RadioGroup, true, ""));

        let selected = self
            .options
            .iter()
            .position(|(v, _)| v == self.value)
            .unwrap_or(0);
        let mut target = selected;
        let mut hovered = None;
        for (i, (value, label)) in self.options.iter().enumerate() {
            let seg = Rect::from_min_size(
                pos2(rect.left() + lefts[i], rect.top() + pad),
                vec2(widths[i], height),
            );
            let r = ui
                .interact(seg, response.id.with(i), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand);
            let selected = *self.value == *value;
            r.widget_info(|| WidgetInfo::selected(egui::Role::RadioButton, true, selected, *label));
            if r.hovered() {
                hovered = Some(i);
            }
            if r.clicked() && *self.value != *value {
                *self.value = *value;
                target = i;
                response.mark_changed();
            }
        }

        if ui.is_rect_visible(rect) && !self.options.is_empty() {
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
            let last = self.options.len().saturating_sub(1) as f32;
            let x = x.clamp(0.0, last);
            let (a, b) = (x.floor() as usize, x.ceil() as usize);
            let f = x - x.floor();
            let lerp = |v: &[f32]| v[a] + (v[b] - v[a]) * f;
            let thumb = Rect::from_min_size(
                pos2(rect.left() + lerp(&lefts), rect.top() + pad),
                vec2(lerp(&widths), height),
            );
            painter.rect_filled(thumb, CornerRadius::same(radius::SM), p.active);
            painter.rect_stroke(
                thumb,
                CornerRadius::same(radius::SM),
                Stroke::new(1.0, p.border_strong),
                StrokeKind::Inside,
            );

            for (i, (_, label)) in self.options.iter().enumerate() {
                let center = pos2(rect.left() + lefts[i] + widths[i] / 2.0, rect.center().y);
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
