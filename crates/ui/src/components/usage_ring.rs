use std::f32::consts::{FRAC_PI_2, TAU};

use egui::epaint::PathShape;
use egui::{
    Align, Align2, Color32, CornerRadius, Layout, Rect, Response, RichText, Sense, Stroke, Ui,
    Vec2, pos2,
};
use mux_core::Provider;
use mux_core::protocol::UsageWindow;

use super::{TooltipExt, provider_color, provider_mark};
use crate::icons;
use crate::theme::{self, Palette, font, icon, radius, space, text};

pub(super) const SIZE: f32 = 12.0;
pub(super) const WIDTH: f32 = 240.0;
pub(super) const BAR: f32 = 6.0;

// one small ring for all of a plan's limits, showing whichever is closest to running out
pub struct UsageRing<'a> {
    windows: &'a [UsageWindow],
    provider: Option<Provider>,
}

impl<'a> UsageRing<'a> {
    pub fn new(windows: &'a [UsageWindow]) -> Self {
        Self {
            windows,
            provider: None,
        }
    }

    pub fn provider(mut self, provider: Provider) -> Self {
        self.provider = Some(provider);
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let used = self
            .windows
            .iter()
            .map(|w| w.used.clamp(0.0, 1.0))
            .fold(0.0, f32::max);
        let label = format!("{}%", (used * 100.0).round() as u32);
        let galley =
            ui.painter()
                .layout_no_wrap(label, font::regular(text::SM + 1.0), p.text_faint);
        let size = Vec2::new(
            SIZE + space::XS + galley.size().x,
            SIZE.max(galley.size().y),
        );
        let (rect, response) = ui.allocate_exact_size(size, Sense::hover());

        if ui.is_rect_visible(rect) {
            ring(ui, pos2(rect.left() + SIZE / 2.0, rect.center().y), used);
            ui.painter().galley(
                pos2(
                    rect.left() + SIZE + space::XS,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                p.text_faint,
            );
        }

        let windows = self.windows;
        let provider = self.provider;
        response.tip_ui(|ui| details(ui, provider, windows))
    }
}

// the hover card, two lines per limit: its name and when it refills, then a bar with the share used
fn details(ui: &mut Ui, provider: Option<Provider>, windows: &[UsageWindow]) {
    let p = theme::palette(ui);
    ui.set_width(WIDTH);
    ui.spacing_mut().item_spacing = Vec2::new(space::SM - 2.0, 0.0);
    // rows hug their text instead of growing to a button's height
    ui.spacing_mut().interact_size.y = 0.0;
    ui.horizontal(|ui| {
        if let Some(provider) = provider {
            let (rect, _) = ui.allocate_exact_size(Vec2::splat(icon::SM), Sense::hover());
            provider_mark(
                ui.painter(),
                rect.center(),
                icon::SM,
                provider,
                provider_color(provider, p.text_muted),
            );
        }
        let title = provider.map_or("Plan usage".to_owned(), |p| format!("{} usage", p.label()));
        ui.label(
            RichText::new(title)
                .font(font::medium(text::SM))
                .color(p.text_muted),
        );
    });
    for (i, window) in windows.iter().enumerate() {
        ui.add_space(if i == 0 { space::MD } else { space::LG });
        let used = window.used.clamp(0.0, 1.0);
        let (glyph, name) = limit_name(&window.label);
        ui.horizontal(|ui| {
            let (slot, _) = ui.allocate_exact_size(Vec2::splat(icon::SM), Sense::hover());
            let name = ui.label(
                RichText::new(name)
                    .font(font::medium(text::MD))
                    .color(p.text),
            );
            // icon glyphs sit high in their line box, so this one is centred on the name instead
            ui.painter().text(
                pos2(slot.center().x, name.rect.center().y),
                Align2::CENTER_CENTER,
                glyph,
                font::icon(icon::SM),
                p.text_faint,
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let reset = resets_in(window.resets_at);
                if !reset.is_empty() {
                    ui.label(
                        RichText::new(format!("resets in {reset}"))
                            .font(font::regular(text::SM))
                            .color(p.text_faint),
                    );
                }
            });
        });
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            let percent = ui.painter().layout_no_wrap(
                format!("{}%", (used * 100.0).round() as u32),
                font::medium(text::SM),
                level_color(p, used),
            );
            let label_w = 30.0;
            let (row, _) = ui.allocate_exact_size(Vec2::new(WIDTH, BAR), Sense::hover());
            bar(
                ui,
                Rect::from_min_max(row.min, pos2(row.right() - label_w, row.bottom())),
                used,
            );
            ui.painter().galley(
                pos2(
                    row.right() - percent.size().x,
                    row.center().y - percent.size().y / 2.0,
                ),
                percent,
                p.text,
            );
        });
    }
}

// every limit of a plan laid out in full, for the settings page
pub fn usage_rows(ui: &mut Ui, windows: &[UsageWindow]) {
    let p = theme::palette(ui);
    for (i, window) in windows.iter().enumerate() {
        if i > 0 {
            let rect = ui.available_rect_before_wrap();
            ui.painter()
                .hline(rect.x_range(), rect.top(), Stroke::new(1.0, p.border));
        }
        let used = window.used.clamp(0.0, 1.0);
        let (glyph, name) = limit_name(&window.label);
        let (rect, _) =
            ui.allocate_exact_size(Vec2::new(ui.available_width(), 76.0), Sense::hover());
        let painter = ui.painter();
        let top = rect.top() + 16.0;
        painter.text(
            pos2(rect.left() + icon::MD / 2.0, top + 8.0),
            Align2::CENTER_CENTER,
            glyph,
            font::icon(icon::MD),
            p.text_muted,
        );
        let left = rect.left() + icon::MD + space::SM;
        painter.text(
            pos2(left, top),
            Align2::LEFT_TOP,
            name,
            font::medium(text::MD),
            p.text,
        );
        let reset = resets_in(window.resets_at);
        let detail = if reset.is_empty() {
            "Reset time not reported".to_owned()
        } else {
            format!("Resets in {reset}")
        };
        painter.text(
            pos2(left, top + 20.0),
            Align2::LEFT_TOP,
            detail,
            font::regular(text::SM + 1.0),
            p.text_faint,
        );
        painter.text(
            pos2(rect.right(), top + 8.0),
            Align2::RIGHT_CENTER,
            format!("{}% used", (used * 100.0).round() as u32),
            font::medium(text::MD),
            level_color(p, used),
        );
        let track = Rect::from_min_max(
            pos2(left, rect.bottom() - 16.0 - BAR),
            pos2(rect.right(), rect.bottom() - 16.0),
        );
        bar(ui, track, used);
    }
}

fn limit_name(label: &str) -> (char, String) {
    match label {
        "5h" => (icons::CLOCK, "5-hour limit".to_owned()),
        "7d" => (icons::CALENDAR_BLANK, "Weekly limit".to_owned()),
        other => (icons::CLOCK, format!("{other} limit")),
    }
}

// a thin ring that fills clockwise from the top as `used` goes from 0 to 1
pub(super) fn ring(ui: &Ui, center: egui::Pos2, used: f32) {
    let p = theme::palette(ui);
    let radius = SIZE / 2.0 - 1.0;
    ui.painter()
        .circle_stroke(center, radius, Stroke::new(2.0, p.border_strong));
    if used > 0.0 {
        let steps = ((used * 48.0).ceil() as usize).max(2);
        let points = (0..=steps)
            .map(|i| {
                let angle = -FRAC_PI_2 + TAU * used * i as f32 / steps as f32;
                center + Vec2::angled(angle) * radius
            })
            .collect();
        ui.painter().add(PathShape::line(
            points,
            Stroke::new(2.0, level_color(p, used)),
        ));
    }
}

pub(super) fn bar(ui: &Ui, rect: Rect, used: f32) {
    let p = theme::palette(ui);
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::FULL), theme::tint(p, 0.10));
    if used > 0.0 {
        let fill = Rect::from_min_size(
            rect.min,
            Vec2::new(
                (rect.width() * used).max(rect.height() * 2.0),
                rect.height(),
            ),
        );
        ui.painter()
            .rect_filled(fill, CornerRadius::same(radius::FULL), level_color(p, used));
    }
}

// close to the limit is worth noticing, so the colour warms up as it fills
pub(super) fn level_color(p: &Palette, used: f32) -> Color32 {
    if used >= 0.9 {
        p.danger
    } else if used >= 0.75 {
        p.warning
    } else {
        p.text_muted
    }
}

fn resets_in(at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let Some(left) = at.checked_sub(now).filter(|_| at > 0) else {
        return String::new();
    };
    let (days, hours, minutes) = (left / 86_400, left % 86_400 / 3600, left % 3600 / 60);
    match (days, hours) {
        (0, 0) => format!("{minutes}m"),
        (0, h) => format!("{h}h {minutes}m"),
        (d, h) => format!("{d}d {h}h"),
    }
}
