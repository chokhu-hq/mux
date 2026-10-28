use egui::{Align2, CornerRadius, Response, Sense, Ui, Vec2, WidgetInfo, pos2};

use super::hover_t;
use crate::icons;
use crate::theme::{self, control, font, icon, radius, space, text};

const MIN_WIDTH: f32 = 180.0;
const BAR: f32 = 3.0;
const DETAIL_HEIGHT: f32 = 44.0;
const BAR_GAP: f32 = 1.5;

pub struct MenuItem<'a> {
    icon: Option<char>,
    // a level out of a total, shown as rising bars, for scales like reasoning effort
    meter: Option<(usize, usize)>,
    label: &'a str,
    // a faint second line under the label, for choices that need a word of explanation
    detail: &'a str,
    danger: bool,
    checked: bool,
}

impl<'a> MenuItem<'a> {
    pub fn new(label: &'a str) -> Self {
        Self {
            icon: None,
            meter: None,
            label,
            detail: "",
            danger: false,
            checked: false,
        }
    }

    pub fn meter(mut self, level: usize, total: usize) -> Self {
        self.meter = Some((level, total.max(1)));
        self
    }

    pub fn detail(mut self, detail: &'a str) -> Self {
        self.detail = detail;
        self
    }

    pub fn icon(mut self, icon: char) -> Self {
        self.icon = Some(icon);
        self
    }

    // marks the current choice in a picker
    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let p = theme::palette(ui);
        let icon_w = match (self.icon, self.meter) {
            (Some(_), _) => icon::MD + space::SM,
            (None, Some((_, total))) => meter_width(total) + space::SM,
            (None, None) => 0.0,
        };
        let label_w = ui
            .painter()
            .layout_no_wrap(self.label.to_owned(), font::regular(text::MD), p.text)
            .size()
            .x
            .max(
                ui.painter()
                    .layout_no_wrap(self.detail.to_owned(), font::regular(text::SM), p.text)
                    .size()
                    .x,
            );
        let check_w = if self.checked {
            icon::MD + space::SM
        } else {
            0.0
        };
        let natural = MIN_WIDTH.max(space::SM * 3.0 + icon_w + label_w + check_w);
        // menus measure on a sizing pass first, then every row fills the settled width
        let width = if ui.is_sizing_pass() {
            natural
        } else {
            ui.available_width().max(natural)
        };
        let height = if self.detail.is_empty() {
            control::MD
        } else {
            DETAIL_HEIGHT
        };
        let size = Vec2::new(width, height);
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
            } else if let Some((level, total)) = self.meter {
                for i in 0..total {
                    let height = 4.0 + 8.0 * (i as f32 + 1.0) / total as f32;
                    let bar = egui::Rect::from_min_size(
                        pos2(
                            x + i as f32 * (BAR + BAR_GAP),
                            rect.center().y + 6.0 - height,
                        ),
                        Vec2::new(BAR, height),
                    );
                    let color = if i < level {
                        p.text_muted
                    } else {
                        p.border_strong
                    };
                    ui.painter().rect_filled(bar, CornerRadius::same(1), color);
                }
                x += meter_width(total) + space::SM;
            }
            if self.detail.is_empty() {
                ui.painter().text(
                    pos2(x, rect.center().y),
                    Align2::LEFT_CENTER,
                    self.label,
                    font::regular(text::MD),
                    fg,
                );
            } else {
                ui.painter().text(
                    pos2(x, rect.center().y - 1.0),
                    Align2::LEFT_BOTTOM,
                    self.label,
                    font::regular(text::MD),
                    fg,
                );
                ui.painter().text(
                    pos2(x, rect.center().y + 1.0),
                    Align2::LEFT_TOP,
                    self.detail,
                    font::regular(text::SM),
                    p.text_faint,
                );
            }
            if self.checked {
                ui.painter().text(
                    pos2(rect.right() - space::SM, rect.center().y),
                    Align2::RIGHT_CENTER,
                    icons::CHECK,
                    font::icon(icon::MD),
                    p.accent,
                );
            }
        }

        response
    }
}

fn meter_width(total: usize) -> f32 {
    total as f32 * BAR + (total.saturating_sub(1)) as f32 * BAR_GAP
}
