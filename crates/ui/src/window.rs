use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2, ViewportBuilder, ViewportCommand, pos2, vec2,
};

use crate::capture;
use crate::theme::{self, font, radius, text};

pub const TITLEBAR_HEIGHT: f32 = 38.0;
const RESIZE_EDGE: f32 = 6.0;
const LIGHT_RADIUS: f32 = 6.0;
const LIGHT_GAP: f32 = 20.0;
const LIGHTS_X: f32 = 20.0;
// macos draws its own traffic lights, so leave room for them instead
const MACOS_LIGHTS_WIDTH: f32 = 78.0;

pub fn viewport(title: &str, size: Vec2) -> ViewportBuilder {
    let builder = ViewportBuilder::default()
        .with_title(title)
        .with_inner_size(capture::size().unwrap_or(size))
        .with_min_inner_size([480.0, 320.0]);
    if cfg!(target_os = "macos") {
        builder
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false)
    } else {
        builder.with_decorations(false).with_transparent(true)
    }
}

// the area outside the rounded corners has to stay see-through
pub const CLEAR_COLOR: [f32; 4] = [0.0; 4];

pub struct WindowFrame<'a> {
    title: &'a str,
}

impl<'a> WindowFrame<'a> {
    pub fn new(title: &'a str) -> Self {
        Self { title }
    }

    pub fn show<R>(self, ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> R {
        let p = theme::palette(ui);
        let rect = ui.max_rect();
        let (maximized, focused) = ui.input(|i| {
            (
                i.viewport().maximized.unwrap_or(false),
                i.viewport().focused.unwrap_or(true),
            )
        });
        let custom = !cfg!(target_os = "macos");
        let rounded = custom && !maximized;
        let corner = CornerRadius::same(if rounded { radius::LG } else { 0 });

        ui.painter().rect_filled(rect, corner, p.bg);

        let bar = Rect::from_min_size(rect.min, vec2(rect.width(), TITLEBAR_HEIGHT));
        let bar_response = ui.interact(
            bar,
            ui.scope_id().with("mux-titlebar"),
            Sense::click_and_drag(),
        );
        if bar_response.double_clicked() {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
        } else if bar_response.drag_started() {
            ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
        }
        ui.painter()
            .hline(bar.x_range(), bar.max.y - 0.5, Stroke::new(1.0, p.border));
        ui.painter().text(
            bar.center(),
            egui::Align2::CENTER_CENTER,
            self.title,
            font::medium(text::MD),
            if focused { p.text_muted } else { p.text_faint },
        );
        if custom {
            traffic_lights(ui, bar, maximized, focused);
        }

        let content_rect = Rect::from_min_max(pos2(rect.min.x, bar.max.y), rect.max);
        let mut content = ui.new_child(UiBuilder::new().max_rect(content_rect));
        content.set_clip_rect(content_rect);
        let inner = add_contents(&mut content);

        if rounded {
            ui.painter().rect_stroke(
                rect,
                corner,
                Stroke::new(1.0, p.border_strong),
                StrokeKind::Inside,
            );
            resize_handles(ui, rect);
        }
        inner
    }
}

pub fn titlebar_leading_inset() -> f32 {
    if cfg!(target_os = "macos") {
        MACOS_LIGHTS_WIDTH
    } else {
        LIGHTS_X + LIGHT_GAP * 2.0 + LIGHT_RADIUS + 12.0
    }
}

#[derive(Clone, Copy)]
enum Light {
    Close,
    Minimize,
    Zoom,
}

fn traffic_lights(ui: &mut Ui, bar: Rect, maximized: bool, focused: bool) {
    let p = theme::palette(ui);
    let first = pos2(bar.min.x + LIGHTS_X, bar.center().y);
    let group = Rect::from_center_size(
        first + vec2(LIGHT_GAP, 0.0),
        vec2(
            LIGHT_GAP * 2.0 + LIGHT_RADIUS * 2.0 + 4.0,
            LIGHT_RADIUS * 2.0 + 4.0,
        ),
    );
    let group_hovered = ui.rect_contains_pointer(group);

    for (i, light) in [Light::Close, Light::Minimize, Light::Zoom]
        .into_iter()
        .enumerate()
    {
        let center = first + vec2(LIGHT_GAP * i as f32, 0.0);
        let hit = Rect::from_center_size(center, Vec2::splat(LIGHT_RADIUS * 2.0 + 4.0));
        let response = ui.interact(hit, ui.scope_id().with(("mux-light", i)), Sense::click());

        let color = match light {
            Light::Close => Color32::from_rgb(0xFF, 0x5F, 0x57),
            Light::Minimize => Color32::from_rgb(0xFE, 0xBC, 0x2E),
            Light::Zoom => Color32::from_rgb(0x28, 0xC8, 0x40),
        };
        let fill = if focused || group_hovered {
            color
        } else {
            p.active
        };
        let fill = if response.is_pointer_button_down_on() {
            fill.gamma_multiply(0.8)
        } else {
            fill
        };
        ui.painter().circle(
            center,
            LIGHT_RADIUS,
            fill,
            Stroke::new(0.5, Color32::from_black_alpha(40)),
        );
        if group_hovered {
            glyph(ui, light, center);
        }

        if response.clicked() {
            let cmd = match light {
                Light::Close => ViewportCommand::Close,
                Light::Minimize => ViewportCommand::Minimized(true),
                Light::Zoom => ViewportCommand::Maximized(!maximized),
            };
            ui.ctx().send_viewport_cmd(cmd);
        }
    }
}

fn glyph(ui: &Ui, light: Light, c: Pos2) {
    let stroke = Stroke::new(1.2, Color32::from_black_alpha(150));
    let painter = ui.painter();
    let r = 2.6;
    match light {
        Light::Close => {
            painter.line_segment([c + vec2(-r, -r), c + vec2(r, r)], stroke);
            painter.line_segment([c + vec2(-r, r), c + vec2(r, -r)], stroke);
        }
        Light::Minimize => {
            painter.line_segment([c + vec2(-r - 0.6, 0.0), c + vec2(r + 0.6, 0.0)], stroke);
        }
        Light::Zoom => {
            let fill = Color32::from_black_alpha(150);
            let a = 3.0;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + vec2(-a, -a),
                    c + vec2(a * 0.5, -a),
                    c + vec2(-a, a * 0.5),
                ],
                fill,
                Stroke::NONE,
            ));
            painter.add(egui::Shape::convex_polygon(
                vec![c + vec2(a, a), c + vec2(-a * 0.5, a), c + vec2(a, -a * 0.5)],
                fill,
                Stroke::NONE,
            ));
        }
    }
}

fn resize_handles(ui: &mut Ui, rect: Rect) {
    let e = RESIZE_EDGE;
    let (l, r, t, b) = (rect.min.x, rect.max.x, rect.min.y, rect.max.y);
    let zones = [
        (
            ResizeDirection::NorthWest,
            CursorIcon::ResizeNorthWest,
            Rect::from_min_max(pos2(l, t), pos2(l + e, t + e)),
        ),
        (
            ResizeDirection::NorthEast,
            CursorIcon::ResizeNorthEast,
            Rect::from_min_max(pos2(r - e, t), pos2(r, t + e)),
        ),
        (
            ResizeDirection::SouthWest,
            CursorIcon::ResizeSouthWest,
            Rect::from_min_max(pos2(l, b - e), pos2(l + e, b)),
        ),
        (
            ResizeDirection::SouthEast,
            CursorIcon::ResizeSouthEast,
            Rect::from_min_max(pos2(r - e, b - e), pos2(r, b)),
        ),
        (
            ResizeDirection::North,
            CursorIcon::ResizeNorth,
            Rect::from_min_max(pos2(l + e, t), pos2(r - e, t + e)),
        ),
        (
            ResizeDirection::South,
            CursorIcon::ResizeSouth,
            Rect::from_min_max(pos2(l + e, b - e), pos2(r - e, b)),
        ),
        (
            ResizeDirection::West,
            CursorIcon::ResizeWest,
            Rect::from_min_max(pos2(l, t + e), pos2(l + e, b - e)),
        ),
        (
            ResizeDirection::East,
            CursorIcon::ResizeEast,
            Rect::from_min_max(pos2(r - e, t + e), pos2(r, b - e)),
        ),
    ];
    for (i, (dir, cursor, zone)) in zones.into_iter().enumerate() {
        let response = ui.interact(zone, ui.scope_id().with(("mux-resize", i)), Sense::drag());
        if response.drag_started() {
            ui.ctx()
                .send_viewport_cmd(ViewportCommand::BeginResize(dir));
        }
        response.on_hover_cursor(cursor);
    }
}
