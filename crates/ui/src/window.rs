use egui::{
    Color32, CornerRadius, CursorIcon, Pos2, Rect, ResizeDirection, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2, ViewportBuilder, ViewportCommand, pos2, vec2,
};

use crate::capture;
use crate::theme::{self, font, radius, text};

pub const TITLEBAR_HEIGHT: f32 = 38.0;
const RESIZE_EDGE: f32 = 6.0;
// macos draws its own traffic lights, so leave room for them instead
const MACOS_LIGHTS_WIDTH: f32 = 78.0;
// windows caption buttons fill the bar's height and are this wide, like every windows app
const CAPTION_WIDTH: f32 = 46.0;
// linux buttons are small circles with a gap between them, like gnome's and kde's
const ROUND_SIZE: f32 = 24.0;
const ROUND_GAP: f32 = 8.0;
// space between the window's edge and its first control
const EDGE_PAD: f32 = 12.0;

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
    sidebar: f32,
    // how solid the sidebar and the content are, below 1 where the compositor blurs behind the window
    opacity: (f32, f32),
}

impl<'a> WindowFrame<'a> {
    pub fn new(title: &'a str) -> Self {
        Self {
            title,
            sidebar: 0.0,
            opacity: (1.0, 1.0),
        }
    }

    // paints a sidebar of this width from the top edge down, behind the traffic lights like macos
    pub fn sidebar(mut self, width: f32) -> Self {
        self.sidebar = width;
        self
    }

    pub fn opacity(mut self, sidebar: f32, content: f32) -> Self {
        self.opacity = (sidebar, content);
        self
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

        let edge = rect.min.x + self.sidebar;
        let r = if rounded { radius::LG } else { 0 };
        let (sidebar_opacity, content_opacity) = self.opacity;
        let see_through = sidebar_opacity < 1.0 && self.sidebar > 0.5;
        if see_through {
            // the background stops at the sidebar so the blur behind it isn't covered twice
            let content = Rect::from_min_max(pos2(edge, rect.min.y), rect.max);
            let corners = CornerRadius {
                nw: 0,
                sw: 0,
                ne: r,
                se: r,
            };
            ui.painter()
                .rect_filled(content, corners, p.bg.gamma_multiply(content_opacity));
        } else {
            ui.painter()
                .rect_filled(rect, corner, p.bg.gamma_multiply(content_opacity));
        }
        if self.sidebar > 0.5 {
            let side = Rect::from_min_max(rect.min, pos2(edge, rect.max.y));
            let corners = CornerRadius {
                nw: r,
                sw: r,
                ne: 0,
                se: 0,
            };
            let fill = p.sidebar.gamma_multiply(sidebar_opacity);
            ui.painter().rect_filled(side, corners, fill);
            ui.painter()
                .vline(edge - 0.5, rect.y_range(), Stroke::new(1.0, p.border));
        }

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
        let content_x = edge.max(bar.min.x)..=bar.max.x;
        ui.painter()
            .hline(content_x, bar.max.y - 0.5, Stroke::new(1.0, p.border));
        if !self.title.is_empty() {
            ui.painter().text(
                bar.center(),
                egui::Align2::CENTER_CENTER,
                self.title,
                font::medium(text::MD),
                if focused { p.text_muted } else { p.text_faint },
            );
        }
        if custom {
            window_buttons(ui, bar, maximized, focused, r);
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

// where the title bar's own controls can start, after any window buttons on the left
pub fn titlebar_leading_inset() -> f32 {
    if cfg!(target_os = "macos") {
        return MACOS_LIGHTS_WIDTH;
    }
    let buttons = &layout().left;
    EDGE_PAD + side_width(buttons) + if buttons.is_empty() { 0.0 } else { EDGE_PAD }
}

// how much of the bar's right end the window buttons take, so the title bar's controls stop short of them
pub fn titlebar_trailing_inset() -> f32 {
    if cfg!(target_os = "macos") {
        return 0.0;
    }
    side_width(&layout().right)
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Button {
    Minimize,
    Maximize,
    Close,
}

#[derive(Default, Debug, PartialEq)]
struct Layout {
    left: Vec<Button>,
    right: Vec<Button>,
}

// windows always puts its buttons at the right, and linux desktops say where theirs go
fn layout() -> &'static Layout {
    static LAYOUT: std::sync::OnceLock<Layout> = std::sync::OnceLock::new();
    LAYOUT.get_or_init(|| {
        let fallback = Layout {
            left: Vec::new(),
            right: vec![Button::Minimize, Button::Maximize, Button::Close],
        };
        if !cfg!(target_os = "linux") {
            return fallback;
        }
        // gnome keeps the layout here, and kde copies its own into the same setting
        let output = mux_core::command("gsettings")
            .args(["get", "org.gnome.desktop.wm.preferences", "button-layout"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        output
            .map(|text| parse_layout(&text))
            .filter(|layout| !layout.left.is_empty() || !layout.right.is_empty())
            .unwrap_or(fallback)
    })
}

// "icon:minimize,maximize,close" puts what's before the colon on the left and the rest on the right
fn parse_layout(text: &str) -> Layout {
    let text = text.trim().trim_matches('\'');
    let (left, right) = text.split_once(':').unwrap_or(("", text));
    let buttons = |side: &str| {
        side.split(',')
            .filter_map(|name| match name.trim() {
                "minimize" => Some(Button::Minimize),
                "maximize" => Some(Button::Maximize),
                "close" => Some(Button::Close),
                // app menus, icons and spacers aren't window buttons
                _ => None,
            })
            .collect()
    };
    Layout {
        left: buttons(left),
        right: buttons(right),
    }
}

fn side_width(buttons: &[Button]) -> f32 {
    if buttons.is_empty() {
        return 0.0;
    }
    if cfg!(target_os = "windows") {
        CAPTION_WIDTH * buttons.len() as f32
    } else {
        let count = buttons.len() as f32;
        ROUND_SIZE * count + ROUND_GAP * (count - 1.0) + EDGE_PAD
    }
}

fn window_buttons(ui: &mut Ui, bar: Rect, maximized: bool, focused: bool, corner: u8) {
    let layout = layout();
    if cfg!(target_os = "windows") {
        caption_buttons(ui, bar, &layout.right, maximized, focused, corner);
    } else {
        let right = bar.right() - EDGE_PAD;
        round_buttons(
            ui,
            bar,
            &layout.right,
            right - side_width(&layout.right) + EDGE_PAD,
            maximized,
        );
        round_buttons(ui, bar, &layout.left, bar.left() + EDGE_PAD, maximized);
    }
}

fn command(button: Button, maximized: bool) -> ViewportCommand {
    match button {
        Button::Minimize => ViewportCommand::Minimized(true),
        Button::Maximize => ViewportCommand::Maximized(!maximized),
        Button::Close => ViewportCommand::Close,
    }
}

// windows 11's caption buttons: flat blocks the bar's height, with close going red under the pointer
fn caption_buttons(
    ui: &mut Ui,
    bar: Rect,
    buttons: &[Button],
    maximized: bool,
    focused: bool,
    corner: u8,
) {
    let p = theme::palette(ui);
    let mut x = bar.right() - CAPTION_WIDTH * buttons.len() as f32;
    for (i, &button) in buttons.iter().enumerate() {
        let rect = Rect::from_min_size(pos2(x, bar.top()), vec2(CAPTION_WIDTH, bar.height()));
        x += CAPTION_WIDTH;
        let response = ui.interact(rect, ui.scope_id().with(("mux-caption", i)), Sense::click());
        let hovered = response.hovered();
        let pressed = response.is_pointer_button_down_on();
        let close = button == Button::Close;
        let fill = match (close, hovered, pressed) {
            (true, true, false) => Color32::from_rgb(0xC4, 0x2B, 0x1C),
            (true, true, true) => Color32::from_rgb(0xC4, 0x2B, 0x1C).gamma_multiply(0.85),
            (false, true, false) => p.hover,
            (false, true, true) => p.active,
            _ => Color32::TRANSPARENT,
        };
        // the last button sits in the window's rounded corner, so its fill follows that curve
        let last = i + 1 == buttons.len();
        let rounding = CornerRadius {
            ne: if last { corner } else { 0 },
            ..CornerRadius::ZERO
        };
        ui.painter().rect_filled(rect, rounding, fill);
        let color = if close && hovered {
            Color32::WHITE
        } else if focused {
            p.text
        } else {
            p.text_faint
        };
        caption_glyph(ui, button, rect.center(), maximized, color);
        if response.clicked() {
            ui.ctx().send_viewport_cmd(command(button, maximized));
        }
    }
}

fn caption_glyph(ui: &Ui, button: Button, c: Pos2, maximized: bool, color: Color32) {
    let stroke = Stroke::new(1.0, color);
    let painter = ui.painter();
    let r = 5.0;
    match button {
        Button::Minimize => {
            painter.line_segment([c + vec2(-r, 0.0), c + vec2(r, 0.0)], stroke);
        }
        Button::Maximize if maximized => {
            // restore is two squares, the back one peeking out above and to the right
            let front = Rect::from_center_size(c + vec2(-1.0, 1.0), Vec2::splat(r * 1.6));
            painter.rect_stroke(front, CornerRadius::same(1), stroke, StrokeKind::Middle);
            let back = front.translate(vec2(2.0, -2.0));
            painter.line_segment([back.left_top(), back.right_top()], stroke);
            painter.line_segment([back.right_top(), back.right_bottom()], stroke);
        }
        Button::Maximize => {
            let square = Rect::from_center_size(c, Vec2::splat(r * 2.0));
            painter.rect_stroke(square, CornerRadius::same(1), stroke, StrokeKind::Middle);
        }
        Button::Close => {
            painter.line_segment([c + vec2(-r, -r), c + vec2(r, r)], stroke);
            painter.line_segment([c + vec2(-r, r), c + vec2(r, -r)], stroke);
        }
    }
}

// gnome and kde's buttons: small circles with a quiet fill that firms up under the pointer
fn round_buttons(ui: &mut Ui, bar: Rect, buttons: &[Button], left: f32, maximized: bool) {
    let p = theme::palette(ui);
    let mut x = left;
    for (i, &button) in buttons.iter().enumerate() {
        let rect = Rect::from_min_size(
            pos2(x, bar.center().y - ROUND_SIZE / 2.0),
            Vec2::splat(ROUND_SIZE),
        );
        x += ROUND_SIZE + ROUND_GAP;
        let id = ui.scope_id().with(("mux-round", left as i32, i));
        let response = ui.interact(rect, id, Sense::click());
        let fill = if response.is_pointer_button_down_on() {
            theme::tint(p, theme::fill::DISABLED)
        } else if response.hovered() {
            theme::tint(p, theme::fill::SELECTED)
        } else {
            theme::tint(p, theme::fill::HOVER)
        };
        ui.painter()
            .circle_filled(rect.center(), ROUND_SIZE / 2.0, fill);
        round_glyph(ui, button, rect.center(), maximized, p.text_muted);
        if response.clicked() {
            ui.ctx().send_viewport_cmd(command(button, maximized));
        }
    }
}

fn round_glyph(ui: &Ui, button: Button, c: Pos2, maximized: bool, color: Color32) {
    let stroke = Stroke::new(1.4, color);
    let painter = ui.painter();
    let r = 3.6;
    match button {
        Button::Minimize => {
            painter.line_segment([c + vec2(-r, r * 0.6), c + vec2(r, r * 0.6)], stroke);
        }
        Button::Maximize => {
            let side = if maximized { r * 1.5 } else { r * 2.0 };
            let square = Rect::from_center_size(c, Vec2::splat(side));
            painter.rect_stroke(square, CornerRadius::same(1), stroke, StrokeKind::Middle);
        }
        Button::Close => {
            painter.line_segment([c + vec2(-r, -r), c + vec2(r, r)], stroke);
            painter.line_segment([c + vec2(-r, r), c + vec2(r, -r)], stroke);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_which_side_each_window_button_goes() {
        assert_eq!(
            parse_layout("'icon:minimize,maximize,close'\n"),
            Layout {
                left: vec![],
                right: vec![Button::Minimize, Button::Maximize, Button::Close],
            }
        );
        // gnome's default shows only close
        assert_eq!(
            parse_layout("'appmenu:close'"),
            Layout {
                left: vec![],
                right: vec![Button::Close],
            }
        );
        assert_eq!(
            parse_layout("'close,minimize,maximize:'"),
            Layout {
                left: vec![Button::Close, Button::Minimize, Button::Maximize],
                right: vec![],
            }
        );
    }
}
