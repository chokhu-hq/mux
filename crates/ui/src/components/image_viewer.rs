use egui::{
    Align2, Color32, Context, CornerRadius, CursorIcon, Event, Id, Image, Key, Margin, Order, Pos2,
    Rect, Sense, Stroke, Ui, UiBuilder, Vec2, pos2, vec2,
};

use super::{IconButton, TooltipExt};
use crate::icons;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, control, font, icon, radius, space, text};

const MIN_ZOOM: f32 = 0.05;
const MAX_ZOOM: f32 = 16.0;
const STEP: f32 = 1.25;
const BAR: f32 = 52.0;
const TOOLBAR: f32 = 64.0;
const ANIMATE: f32 = 0.14;

#[derive(Clone)]
struct State {
    paths: Vec<String>,
    index: usize,
    open: bool,
    // None fits the picture in the window, otherwise screen points per picture pixel
    zoom: Option<f32>,
    pan: Vec2,
}

// pictures from the chat open over the whole window, with zoom and pan, instead of in another app
pub struct ImageViewer;

impl ImageViewer {
    pub fn open(ctx: &Context, paths: Vec<String>, index: usize) {
        if paths.is_empty() {
            return;
        }
        let index = index.min(paths.len() - 1);
        let state = State {
            paths,
            index,
            open: true,
            zoom: None,
            pan: Vec2::ZERO,
        };
        ctx.data_mut(|d| d.insert_temp(id(), state));
    }

    pub fn is_open(ctx: &Context) -> bool {
        ctx.data(|d| d.get_temp::<State>(id()))
            .is_some_and(|s| s.open)
    }

    // shown once a frame before the rest of the app, so its keys never reach the chat underneath
    pub fn show(ctx: &Context) {
        let Some(mut state) = ctx.data(|d| d.get_temp::<State>(id())) else {
            return;
        };
        let t = spring_bool(ctx, id().with("open"), state.open, Spring::SNAPPY).clamp(0.0, 1.0);
        if !state.open && t <= 0.001 {
            ctx.data_mut(|d| d.remove::<State>(id()));
            return;
        }
        if state.open {
            keys(ctx, &mut state);
        }
        let screen = ctx.content_rect();
        egui::Area::new(id().with("area"))
            .order(Order::Foreground)
            .fixed_pos(screen.min)
            .constrain(false)
            .interactable(state.open)
            .show(ctx, |ui| {
                ui.set_clip_rect(screen);
                ui.multiply_opacity(t);
                viewer(ui, screen, &mut state, t);
            });
        ctx.data_mut(|d| d.insert_temp(id(), state));
    }
}

fn id() -> Id {
    Id::unique("mux-image-viewer")
}

fn uri(path: &str) -> String {
    format!("file://{path}")
}

fn keys(ctx: &Context, state: &mut State) {
    let count = state.paths.len();
    ctx.input_mut(|i| {
        if i.consume_key(egui::Modifiers::NONE, Key::Escape) {
            state.open = false;
        }
        if i.consume_key(egui::Modifiers::NONE, Key::ArrowRight) && count > 1 {
            go(state, state.index + 1);
        }
        if i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft) && count > 1 {
            go(state, state.index + count - 1);
        }
        let zoom_in = i
            .events
            .iter()
            .any(|e| matches!(e, Event::Text(t) if t == "+" || t == "="));
        let zoom_out = i
            .events
            .iter()
            .any(|e| matches!(e, Event::Text(t) if t == "-"));
        let fit = i
            .events
            .iter()
            .any(|e| matches!(e, Event::Text(t) if t == "0"));
        // the viewer owns the keyboard while open, so nothing typed lands in the composer behind it
        i.events
            .retain(|e| !matches!(e, Event::Key { .. } | Event::Text(_) | Event::Paste(_)));
        let pending = if zoom_in {
            Some(Some(STEP))
        } else if zoom_out {
            Some(Some(1.0 / STEP))
        } else if fit {
            Some(None)
        } else {
            None
        };
        if let Some(step) = pending {
            ctx.data_mut(|d| d.insert_temp(id().with("key-zoom"), step));
        }
    });
}

fn go(state: &mut State, index: usize) {
    state.index = index % state.paths.len();
    state.zoom = None;
    state.pan = Vec2::ZERO;
}

fn viewer(ui: &mut Ui, screen: Rect, state: &mut State, t: f32) {
    let ctx = ui.ctx().clone();
    ui.painter()
        .rect_filled(screen, CornerRadius::ZERO, Color32::from_black_alpha(242));
    let view = Rect::from_min_max(
        pos2(screen.left() + space::LG, screen.top() + BAR),
        pos2(screen.right() - space::LG, screen.bottom() - TOOLBAR),
    );
    let backdrop = ui.interact(screen, id().with("backdrop"), Sense::click_and_drag());

    let path = state.paths[state.index].clone();
    let image = Image::new(uri(&path)).fit_to_original_size(1.0);
    let poll = image.load_for_size(&ctx, view.size());
    let loading = poll.as_ref().is_ok_and(|poll| poll.size().is_none());
    let texture = poll
        .ok()
        .and_then(|poll| poll.texture_id().zip(poll.size()));

    let ppp = ctx.pixels_per_point();
    let natural = texture.map(|(_, size)| size / ppp);
    let fit = natural.map_or(1.0, |n| {
        (view.width() / n.x).min(view.height() / n.y).min(1.0)
    });
    let zoom = state.zoom.unwrap_or(fit);

    let pointer = ui.input(|i| i.pointer.hover_pos());
    let over_view = pointer.is_some_and(|pos| view.contains(pos));
    let mut instant = false;
    let mut target = zoom;
    let mut anchor = view.center();

    if natural.is_some() && state.open {
        // the wheel and pinches zoom around the pointer, the way map and photo apps do
        let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
        let factor = pinch * (scroll * 0.0025).exp();
        if over_view && (factor - 1.0).abs() > 1e-4 {
            target = zoom * factor;
            anchor = pointer.unwrap_or(anchor);
            instant = true;
        }
        if let Some(step) = ctx.data_mut(|d| d.remove_temp::<Option<f32>>(id().with("key-zoom"))) {
            target = step.map_or(fit, |s| zoom * s);
        }
        if backdrop.double_clicked() {
            let actual = (zoom - 1.0).abs() < 0.01;
            target = if actual { fit } else { 1.0 };
            anchor = pointer.unwrap_or(anchor);
        }
        if backdrop.dragged() {
            state.pan += backdrop.drag_delta();
            instant = true;
        }
    }

    let size = natural.unwrap_or(Vec2::splat(64.0));
    let target = target.clamp(MIN_ZOOM.min(fit), MAX_ZOOM);
    if (target - zoom).abs() > 1e-5 {
        // the point under the anchor stays put while the picture grows or shrinks around it
        let center = view.center() + state.pan;
        let next = anchor - (anchor - center) * (target / zoom);
        state.pan = next - view.center();
        state.zoom = if (target - fit).abs() < 1e-4 {
            None
        } else {
            Some(target)
        };
    }
    let zoom = state.zoom.unwrap_or(fit);
    if state.zoom.is_none() {
        state.pan = Vec2::ZERO;
    }
    state.pan = clamp_pan(state.pan, size * zoom, view.size());

    let duration = if instant { 0.0 } else { ANIMATE };
    let shown_zoom = ctx.animate_value_with_time(id().with("zoom"), zoom, duration);
    let shown_pan = vec2(
        ctx.animate_value_with_time(id().with("pan-x"), state.pan.x, duration),
        ctx.animate_value_with_time(id().with("pan-y"), state.pan.y, duration),
    );

    // settles in from slightly smaller as it opens
    let grow = 0.96 + 0.04 * t;
    let rect = Rect::from_center_size(view.center() + shown_pan, size * shown_zoom * grow);
    let painter = ui.painter().with_clip_rect(view.expand(space::LG));
    match texture {
        Some((texture, _)) => {
            painter.image(
                texture,
                rect,
                Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        None if loading => {
            painter.text(
                view.center(),
                Align2::CENTER_CENTER,
                "Loading…",
                font::regular(text::MD),
                Color32::from_white_alpha(160),
            );
        }
        None => {
            painter.text(
                view.center() - vec2(0.0, 14.0),
                Align2::CENTER_CENTER,
                icons::WARNING,
                font::icon(icon::XL),
                Color32::from_white_alpha(160),
            );
            painter.text(
                view.center() + vec2(0.0, 18.0),
                Align2::CENTER_CENTER,
                "Couldn't load this picture",
                font::regular(text::MD),
                Color32::from_white_alpha(200),
            );
        }
    }

    let movable = (size * zoom).x > view.width() + 0.5 || (size * zoom).y > view.height() + 0.5;
    if backdrop.dragged() && movable {
        ctx.set_cursor_icon(CursorIcon::Grabbing);
    } else if backdrop.hovered() && rect.contains(pointer.unwrap_or_default()) && movable {
        ctx.set_cursor_icon(CursorIcon::Grab);
    }
    // a plain click beside the picture closes, the way lightboxes usually do
    if backdrop.clicked() && !rect.contains(pointer.unwrap_or_default()) {
        state.open = false;
    }

    title(ui, screen, state);
    if state.paths.len() > 1 {
        arrows(ui, view, state);
    }
    toolbar(ui, screen, zoom, fit, &path);
}

// the picture may only be dragged as far as its own overflow, so it can't be lost off screen
fn clamp_pan(pan: Vec2, shown: Vec2, view: Vec2) -> Vec2 {
    let room = ((shown - view) / 2.0).max(Vec2::ZERO);
    vec2(pan.x.clamp(-room.x, room.x), pan.y.clamp(-room.y, room.y))
}

fn title(ui: &mut Ui, screen: Rect, state: &mut State) {
    let path = &state.paths[state.index];
    let name = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
    let y = screen.top() + BAR / 2.0;
    let mut left = screen.left() + space::LG;
    let painter = ui.painter();
    let galley =
        painter.layout_no_wrap(name, font::medium(text::MD), Color32::from_white_alpha(235));
    let width = galley.size().x;
    painter.galley(
        pos2(left, y - galley.size().y / 2.0),
        galley,
        Color32::WHITE,
    );
    left += width + space::SM;
    if state.paths.len() > 1 {
        painter.text(
            pos2(left, y),
            Align2::LEFT_CENTER,
            format!("{} of {}", state.index + 1, state.paths.len()),
            font::regular(text::SM),
            Color32::from_white_alpha(140),
        );
    }
    let close = Rect::from_center_size(
        pos2(screen.right() - space::LG - 14.0, y),
        Vec2::splat(28.0),
    );
    if round(ui, close, icons::X, "Close").clicked() {
        state.open = false;
    }
}

fn arrows(ui: &mut Ui, view: Rect, state: &mut State) {
    let count = state.paths.len();
    let y = view.center().y;
    let back = Rect::from_center_size(pos2(view.left() + 20.0, y), Vec2::splat(36.0));
    let next = Rect::from_center_size(pos2(view.right() - 20.0, y), Vec2::splat(36.0));
    if round(ui, back, icons::CARET_LEFT, "Previous").clicked() {
        go(state, state.index + count - 1);
    }
    if round(ui, next, icons::CARET_RIGHT, "Next").clicked() {
        go(state, state.index + 1);
    }
}

// a soft dark circle that stays readable over any picture
fn round(ui: &mut Ui, rect: Rect, glyph: char, label: &'static str) -> egui::Response {
    let response = ui
        .interact(rect, id().with(label), Sense::click())
        .on_hover_cursor(CursorIcon::PointingHand);
    let t = super::hover_t(ui, &response);
    ui.painter().circle_filled(
        rect.center(),
        rect.width() / 2.0,
        Color32::from_white_alpha((18.0 + 22.0 * t) as u8),
    );
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        glyph,
        font::icon(icon::MD),
        Color32::from_white_alpha((200.0 + 55.0 * t) as u8),
    );
    response.tip(label)
}

fn toolbar(ui: &mut Ui, screen: Rect, zoom: f32, fit: f32, path: &str) {
    let p = theme::palette(ui);
    let center = pos2(screen.center().x, screen.bottom() - TOOLBAR / 2.0);
    let area = Rect::from_center_size(center, vec2(260.0, 40.0));
    let mut bar = ui.new_child(
        UiBuilder::new()
            .max_rect(area)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let frame = theme::floating(ui)
        .corner_radius(CornerRadius::same(radius::XL))
        .inner_margin(Margin::same(space::XS as i8));
    let inner = frame.show(&mut bar, |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let mut step = None;
        if IconButton::new(icons::MAGNIFYING_GLASS_MINUS, "Zoom out")
            .show(ui)
            .tip("Zoom out")
            .clicked()
        {
            step = Some(Some(1.0 / STEP));
        }
        let percent = format!("{}%", (zoom * 100.0).round());
        let (rect, label) = ui.allocate_exact_size(vec2(52.0, control::SM), Sense::click());
        let t = super::hover_t(ui, &label);
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(radius::MD),
            p.hover.gamma_multiply(t),
        );
        ui.painter().text(
            rect.center(),
            Align2::CENTER_CENTER,
            percent,
            font::medium(text::SM),
            p.text,
        );
        if label
            .on_hover_cursor(CursorIcon::PointingHand)
            .tip("Actual size")
            .clicked()
        {
            step = Some(Some(1.0 / zoom));
        }
        if IconButton::new(icons::MAGNIFYING_GLASS_PLUS, "Zoom in")
            .show(ui)
            .tip("Zoom in")
            .clicked()
        {
            step = Some(Some(STEP));
        }
        divider(ui);
        if IconButton::new(icons::CORNERS_OUT, "Fit")
            .selected((zoom - fit).abs() < 1e-4)
            .show(ui)
            .tip("Fit to window")
            .clicked()
        {
            step = Some(None);
        }
        if IconButton::new(icons::ARROW_SQUARE_OUT, "Open")
            .show(ui)
            .tip("Open in the default app")
            .clicked()
        {
            super::chat::open_externally(path);
        }
        step
    });
    if let Some(step) = inner.inner {
        ui.ctx()
            .data_mut(|d| d.insert_temp(id().with("key-zoom"), step));
    }
}

fn divider(ui: &mut Ui) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(9.0, 18.0), Sense::hover());
    ui.painter()
        .vline(rect.center().x, rect.y_range(), Stroke::new(1.0, p.border));
}
