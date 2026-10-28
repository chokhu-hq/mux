use egui::{
    Align2, CornerRadius, CursorIcon, Image, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2,
    pos2, vec2,
};
use mux_core::protocol::is_image;

use crate::components::{TooltipExt, hover_t};
use crate::icons;
use crate::theme::{self, font, icon, radius, space, text};

const TILE: f32 = 56.0;
const FILE_WIDTH: f32 = 200.0;
const PREVIEW: Vec2 = vec2(280.0, 200.0);

// the files waiting in the composer, returning the one whose remove button was clicked
pub fn pending(ui: &mut Ui, paths: &[String]) -> Option<usize> {
    let mut removed = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(space::SM);
        for (i, path) in paths.iter().enumerate() {
            let response = ui.push_id(i, |ui| {
                if is_image(path) {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::splat(TILE), Sense::hover());
                    picture(ui, path, rect, true);
                    response
                } else {
                    file(ui, path, TILE)
                }
            });
            if remove_button(ui, &response.inner) {
                removed = Some(i);
            }
        }
    });
    removed
}

// what was sent with a message, images as previews and other files as tiles
pub fn sent(ui: &mut Ui, paths: &[String]) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.spacing_mut().item_spacing = Vec2::splat(space::SM);
        for (i, path) in paths.iter().enumerate().rev() {
            ui.push_id(i, |ui| {
                if is_image(path) {
                    let size = fitted(ui, path, PREVIEW);
                    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
                    picture(ui, path, rect, false);
                    let response = response
                        .on_hover_cursor(CursorIcon::PointingHand)
                        .tip(mux_core::display_path(path));
                    if response.clicked() {
                        open(path);
                    }
                } else if file(ui, path, 44.0).clicked() {
                    open(path);
                }
            });
        }
    });
}

// a picture from disk drawn into `rect`, cropped to fill it or fitted inside it
pub fn picture(ui: &Ui, path: &str, rect: Rect, cover: bool) {
    let p = theme::palette(ui);
    let rounding = CornerRadius::same(radius::LG);
    let uri = format!("file://{path}");
    let image = Image::new(uri).corner_radius(rounding);
    match image.load_for_size(ui.ctx(), rect.size()) {
        Ok(poll) => match poll.size() {
            Some(source) => {
                let uv = if cover {
                    cover_uv(source, rect.size())
                } else {
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
                };
                image.uv(uv).paint_at(ui, rect);
            }
            None => {
                ui.painter()
                    .rect_filled(rect, rounding, theme::tint(p, 0.06));
            }
        },
        Err(_) => {
            ui.painter()
                .rect_filled(rect, rounding, theme::tint(p, 0.06));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                icons::WARNING,
                font::icon(icon::MD),
                p.text_faint,
            );
        }
    }
    ui.painter().rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
}

// a preview keeps the picture's shape, shrunk to fit inside `max`
pub fn fitted(ui: &Ui, path: &str, max: Vec2) -> Vec2 {
    let uri = format!("file://{path}");
    let source = Image::new(uri)
        .load_for_size(ui.ctx(), max)
        .ok()
        .and_then(|poll| poll.size());
    match source {
        Some(size) if size.x > 0.0 && size.y > 0.0 => {
            let scale = (max.x / size.x).min(max.y / size.y).min(1.0);
            size * scale
        }
        _ => vec2(max.y, max.y),
    }
}

// the part of the picture that fills `target` without stretching, cut evenly from both sides
fn cover_uv(source: Vec2, target: Vec2) -> Rect {
    let (source_ratio, target_ratio) = (source.x / source.y, target.x / target.y);
    if source_ratio > target_ratio {
        let w = target_ratio / source_ratio;
        Rect::from_min_max(pos2((1.0 - w) / 2.0, 0.0), pos2((1.0 + w) / 2.0, 1.0))
    } else {
        let h = source_ratio / target_ratio;
        Rect::from_min_max(pos2(0.0, (1.0 - h) / 2.0), pos2(1.0, (1.0 + h) / 2.0))
    }
}

fn file(ui: &mut Ui, path: &str, height: f32) -> Response {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(FILE_WIDTH, height), Sense::click());
    let t = hover_t(ui, &response);
    let rounding = CornerRadius::same(radius::LG);
    ui.painter()
        .rect_filled(rect, rounding, theme::tint(p, 0.05 + 0.03 * t));
    ui.painter().rect_stroke(
        rect,
        rounding,
        Stroke::new(1.0, p.border),
        StrokeKind::Inside,
    );
    let icon_center = pos2(rect.left() + space::MD + icon::LG / 2.0, rect.center().y);
    ui.painter().text(
        icon_center,
        Align2::CENTER_CENTER,
        icons::FILE,
        font::icon(icon::LG),
        p.text_muted,
    );
    let name = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned());
    let left = icon_center.x + icon::LG / 2.0 + space::SM;
    let mut job =
        egui::text::LayoutJob::simple_singleline(name, font::medium(text::SM + 1.0), p.text);
    job.wrap = egui::text::TextWrapping {
        max_width: rect.right() - space::MD - left,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(left, rect.center().y - galley.size().y / 2.0),
        galley,
        p.text,
    );
    response
        .on_hover_cursor(CursorIcon::PointingHand)
        .tip(mux_core::display_path(path))
}

// a small round x in the tile's corner, shown while the tile is hovered
fn remove_button(ui: &mut Ui, tile: &Response) -> bool {
    let p = theme::palette(ui);
    let center = pos2(tile.rect.right() - 2.0, tile.rect.top() + 2.0);
    let rect = Rect::from_center_size(center, Vec2::splat(18.0));
    let response = ui
        .interact(rect, tile.id.with("remove"), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    let shown = tile.hovered() || response.hovered();
    let t = crate::motion::spring_bool(
        ui.ctx(),
        tile.id.with("remove-t"),
        shown,
        crate::motion::Spring::QUICK,
    );
    if t > 0.01 {
        let painter = ui.painter().clone().with_layer_id(egui::LayerId::new(
            egui::Order::Foreground,
            tile.id.with("remove-layer"),
        ));
        painter.circle_filled(center, 8.0, p.text.gamma_multiply(t));
        painter.text(
            center,
            Align2::CENTER_CENTER,
            icons::X,
            font::icon(10.0),
            p.bg.gamma_multiply(t),
        );
    }
    response.clicked()
}

// hands the file to the desktop's default app for it
pub fn open(path: &str) {
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    let _ = std::process::Command::new(program).arg(path).spawn();
}
