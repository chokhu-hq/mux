use std::borrow::Cow;
use std::time::Duration;

use alacritty_terminal::event::{EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Term, TermMode, point_to_viewport, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};
use egui::{
    Align2, Color32, CursorIcon, Event, EventFilter, FontId, Key, Modifiers, PointerButton, Pos2,
    Rect, Response, Sense, Stroke, StrokeKind, Ui, ViewportCommand, pos2, vec2,
};

use crate::motion;
use crate::theme::{self, Palette, font, text};

pub struct TerminalView<'a, L: EventListener> {
    term: &'a FairMutex<Term<L>>,
    size: f32,
}

// the text size range zooming moves within, and how far one step goes
pub const MIN_SIZE: f32 = 8.0;
pub const MAX_SIZE: f32 = 32.0;
const STEP: f32 = 1.0;

pub struct TerminalResponse {
    pub response: Response,
    pub input: Vec<u8>,
    pub resize: Option<WindowSize>,
    // a new text size asked for by zooming in or out, which the app keeps
    pub zoom: Option<f32>,
}

impl<'a, L: EventListener> TerminalView<'a, L> {
    pub fn new(term: &'a FairMutex<Term<L>>) -> Self {
        Self {
            term,
            size: text::MONO,
        }
    }

    pub fn font_size(mut self, size: f32) -> Self {
        self.size = size.clamp(MIN_SIZE, MAX_SIZE);
        self
    }

    pub fn show(self, ui: &mut Ui) -> TerminalResponse {
        let p = theme::palette(ui);
        let fonts = Fonts {
            regular: font::mono(self.size),
            bold: font::mono_bold(self.size),
            italic: font::mono_italic(self.size),
            bold_italic: font::mono_bold_italic(self.size),
        };
        let (cell_w, cell_h) = ui.ctx().fonts_mut(|f| {
            (
                f.glyph_width(&fonts.regular, 'M'),
                f.row_height(&fonts.regular).ceil(),
            )
        });
        let (rect, response) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        let response = response.on_hover_cursor(CursorIcon::Text);
        if response.clicked() || response.drag_started() {
            response.request_focus();
        }
        let focused = response.has_focus();
        if focused {
            // keep tab, arrows and escape for the shell instead of egui's focus navigation
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
        }

        let cols = ((rect.width() / cell_w).floor() as usize).max(2);
        let lines = ((rect.height() / cell_h).floor() as usize).max(1);
        let mut term = self.term.lock();
        let mut out = TerminalResponse {
            response,
            input: Vec::new(),
            resize: None,
            zoom: None,
        };
        if term.columns() != cols || term.screen_lines() != lines {
            term.resize(TermSize::new(cols, lines));
            out.resize = Some(WindowSize {
                num_lines: lines as u16,
                num_cols: cols as u16,
                cell_width: cell_w as u16,
                cell_height: cell_h as u16,
            });
        }

        let grid = Grid {
            origin: rect.min,
            cell: vec2(cell_w, cell_h),
            cols,
            lines,
        };
        mouse(ui, &out.response, &mut term, &grid, &mut out.input);
        // a program that asked for the mouse gets right clicks itself, unless shift is held as with selecting
        let captured =
            term.mode().intersects(TermMode::MOUSE_MODE) && !ui.input(|i| i.modifiers.shift);
        if !captured {
            menu(&out.response, &term);
        }
        out.zoom = zoom(ui, &out.response, focused, self.size);
        if focused {
            keyboard(ui, out.response.id, &mut term, &mut out.input);
        }

        let cursor = if focused {
            blink(ui, out.response.id, !out.input.is_empty())
        } else {
            Cursor::Unfocused
        };
        if ui.is_rect_visible(rect) {
            paint(ui, p, &term, &grid, cursor, &fonts);
        }
        out
    }
}

const BLINK: f64 = 0.53;

#[derive(Clone, Copy, PartialEq)]
enum Cursor {
    Visible,
    BlinkedOff,
    Unfocused,
}

// restarts the blink on every keystroke so the cursor stays put while typing
fn blink(ui: &Ui, id: egui::Id, typed: bool) -> Cursor {
    let id = id.with("blink");
    let now = ui.input(|i| i.time);
    if typed || motion::reduced(ui.ctx()) {
        ui.data_mut(|d| d.insert_temp(id, now));
    }
    if motion::reduced(ui.ctx()) {
        return Cursor::Visible;
    }
    let start = ui.data_mut(|d| *d.get_temp_mut_or(id, now));
    let since = now - start;
    ui.ctx()
        .request_repaint_after(Duration::from_secs_f64(BLINK - since % BLINK));
    if ((since / BLINK) as u64).is_multiple_of(2) {
        Cursor::Visible
    } else {
        Cursor::BlinkedOff
    }
}

struct Fonts {
    regular: FontId,
    bold: FontId,
    italic: FontId,
    bold_italic: FontId,
}

impl Fonts {
    fn pick(&self, bold: bool, italic: bool) -> &FontId {
        match (bold, italic) {
            (false, false) => &self.regular,
            (true, false) => &self.bold,
            (false, true) => &self.italic,
            (true, true) => &self.bold_italic,
        }
    }
}

struct Grid {
    origin: Pos2,
    cell: egui::Vec2,
    cols: usize,
    lines: usize,
}

impl Grid {
    fn cell_rect(&self, line: usize, col: usize, width: usize) -> Rect {
        Rect::from_min_size(
            self.origin + vec2(col as f32 * self.cell.x, line as f32 * self.cell.y),
            vec2(self.cell.x * width as f32, self.cell.y),
        )
    }

    fn point_at(&self, pos: Pos2, display_offset: usize) -> (Point, Side) {
        let rel = pos - self.origin;
        let x = (rel.x / self.cell.x).max(0.0);
        let col = (x as usize).min(self.cols - 1);
        let line = ((rel.y / self.cell.y).max(0.0) as usize).min(self.lines - 1);
        let side = if x.fract() < 0.5 {
            Side::Left
        } else {
            Side::Right
        };
        (
            viewport_to_point(display_offset, Point::new(line, Column(col))),
            side,
        )
    }

    fn cell_at(&self, pos: Pos2) -> (usize, usize) {
        let rel = pos - self.origin;
        let line = ((rel.y / self.cell.y).max(0.0) as usize).min(self.lines - 1);
        let col = ((rel.x / self.cell.x).max(0.0) as usize).min(self.cols - 1);
        (line, col)
    }
}

fn mouse<L: EventListener>(
    ui: &Ui,
    response: &Response,
    term: &mut Term<L>,
    grid: &Grid,
    input: &mut Vec<u8>,
) {
    let mode = *term.mode();
    // holding shift skips app mouse capture so text can still be selected, like other terminals
    let reporting = mode.intersects(TermMode::MOUSE_MODE) && !ui.input(|i| i.modifiers.shift);
    if reporting {
        report_buttons(ui, response, mode, grid, input);
    } else if let Some(pos) = response.interact_pointer_pos() {
        let (point, side) = grid.point_at(pos, term.grid().display_offset());
        if response.drag_started() {
            term.selection = Some(Selection::new(SelectionType::Simple, point, side));
        } else if response.dragged()
            && let Some(selection) = &mut term.selection
        {
            selection.update(point, side);
        } else if response.triple_clicked() {
            term.selection = Some(Selection::new(SelectionType::Lines, point, side));
        } else if response.double_clicked() {
            term.selection = Some(Selection::new(SelectionType::Semantic, point, side));
        } else if response.clicked() {
            term.selection = None;
        }
    }

    let Some(hover) = response.hover_pos() else {
        return;
    };
    let id = response.id.with("scroll");
    let delta = ui.input(|i| i.smooth_scroll_delta.y);
    let acc = ui.data(|d| d.get_temp::<f32>(id).unwrap_or(0.0)) + delta / grid.cell.y;
    let lines = acc.trunc() as i32;
    ui.data_mut(|d| d.insert_temp(id, acc - lines as f32));
    if lines == 0 {
        return;
    }
    if reporting {
        let code = if lines > 0 { 64 } else { 65 } + modifier_bits(ui.input(|i| i.modifiers));
        for _ in 0..lines.unsigned_abs() {
            report(input, mode, code, grid.cell_at(hover), true);
        }
    } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL) {
        // full screen apps like less and vim scroll with arrow keys in the alternate screen
        let arrow: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
            (true, true) => b"\x1bOA",
            (true, false) => b"\x1b[A",
            (false, true) => b"\x1bOB",
            (false, false) => b"\x1b[B",
        };
        for _ in 0..lines.unsigned_abs() {
            input.extend_from_slice(arrow);
        }
    } else {
        term.scroll_display(Scroll::Delta(lines));
    }
}

fn report_buttons(ui: &Ui, response: &Response, mode: TermMode, grid: &Grid, input: &mut Vec<u8>) {
    let held_id = response.id.with("mouse-held");
    let cell_id = response.id.with("mouse-cell");
    let mut held: Option<u8> = ui.data(|d| d.get_temp(held_id)).flatten();
    let mut last: Option<(usize, usize)> = ui.data(|d| d.get_temp(cell_id)).flatten();
    let rect = response.rect;

    for event in ui.input(|i| i.events.clone()) {
        match event {
            Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers,
            } => {
                let code = match button {
                    PointerButton::Primary => 0,
                    PointerButton::Middle => 1,
                    PointerButton::Secondary => 2,
                    _ => continue,
                };
                if (pressed && !rect.contains(pos)) || (!pressed && held != Some(code)) {
                    continue;
                }
                report(
                    input,
                    mode,
                    code + modifier_bits(modifiers),
                    grid.cell_at(pos),
                    pressed,
                );
                held = pressed.then_some(code);
            }
            Event::PointerMoved(pos) => {
                let tracked = mode.contains(TermMode::MOUSE_MOTION)
                    || (mode.contains(TermMode::MOUSE_DRAG) && held.is_some());
                if !tracked || (held.is_none() && !rect.contains(pos)) {
                    continue;
                }
                let cell = grid.cell_at(pos);
                if last == Some(cell) {
                    continue;
                }
                last = Some(cell);
                let code = held.unwrap_or(3) + 32 + modifier_bits(ui.input(|i| i.modifiers));
                report(input, mode, code, cell, true);
            }
            _ => {}
        }
    }
    ui.data_mut(|d| {
        d.insert_temp(held_id, held);
        d.insert_temp(cell_id, last);
    });
}

fn modifier_bits(m: Modifiers) -> u8 {
    4 * m.shift as u8 + 8 * m.alt as u8 + 16 * m.ctrl as u8
}

fn report(
    input: &mut Vec<u8>,
    mode: TermMode,
    code: u8,
    (line, col): (usize, usize),
    pressed: bool,
) {
    if mode.contains(TermMode::SGR_MOUSE) {
        let end = if pressed { 'M' } else { 'm' };
        input.extend(format!("\x1b[<{code};{};{}{end}", col + 1, line + 1).bytes());
        return;
    }
    // the legacy encoding cannot say which button was released, and caps positions at 223
    let code = if pressed { code } else { (code & !3) | 3 };
    let (x, y) = (33 + col, 33 + line);
    if x <= 255 && y <= 255 {
        input.extend_from_slice(&[0x1b, b'[', b'M', 32 + code, x as u8, y as u8]);
    }
}

fn keyboard<L: EventListener>(ui: &Ui, id: egui::Id, term: &mut Term<L>, input: &mut Vec<u8>) {
    let mac = cfg!(target_os = "macos");
    let paste_id = id.with("paste");
    let events = ui.input(|i| i.events.clone());
    let mode = *term.mode();
    let start = input.len();

    for event in events {
        match event {
            Event::Text(text) => {
                if !mac && ui.input(|i| i.modifiers.alt) {
                    input.push(0x1b);
                }
                input.extend_from_slice(text.as_bytes());
            }
            // egui turns cmd+c (ctrl+c off macos) into copy events, so map them back to control bytes
            Event::Copy if mac => copy(ui, term),
            Event::Copy => input.push(0x03),
            Event::Cut if !mac => input.push(0x18),
            Event::Paste(text) => {
                let requested = ui.data_mut(|d| d.remove_temp::<bool>(paste_id).is_some());
                if mac || requested {
                    paste(&text, mode, input);
                } else {
                    input.push(0x16);
                }
            }
            Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                if !mac && modifiers.ctrl && modifiers.shift && key == Key::C {
                    copy(ui, term);
                } else if !mac && modifiers.ctrl && modifiers.shift && key == Key::V {
                    ui.data_mut(|d| d.insert_temp(paste_id, true));
                    ui.ctx().send_viewport_cmd(ViewportCommand::RequestPaste);
                } else if let Some(bytes) = key_bytes(key, modifiers, mode) {
                    input.extend_from_slice(&bytes);
                }
            }
            _ => {}
        }
    }

    if input.len() > start {
        term.scroll_display(Scroll::Bottom);
        term.selection = None;
    }
}

// the zoom keys change only the terminal's text while it has focus, and so does ctrl and the wheel over it
fn zoom(ui: &Ui, response: &Response, focused: bool, size: f32) -> Option<f32> {
    let mut next = size;
    if focused {
        ui.input_mut(|i| {
            let command = |key| egui::KeyboardShortcut::new(Modifiers::COMMAND, key);
            for key in [Key::Plus, Key::Equals] {
                if i.consume_shortcut(&command(key)) {
                    next += STEP;
                }
            }
            if i.consume_shortcut(&command(Key::Minus)) {
                next -= STEP;
            }
            if i.consume_shortcut(&command(Key::Num0)) {
                next = text::MONO;
            }
        });
    }
    // egui turns ctrl and the wheel into a zoom factor rather than a scroll
    let factor = ui.input(|i| i.zoom_delta());
    if response.hovered() && factor != 1.0 {
        next *= factor;
    }
    let next = (next * 2.0).round() / 2.0;
    (next != size).then_some(next.clamp(MIN_SIZE, MAX_SIZE))
}

// copy what's selected and paste into the shell, on right click
fn menu<L: EventListener>(response: &Response, term: &Term<L>) {
    egui::Popup::context_menu(response).show(|ui| {
        ui.set_min_width(180.0);
        if let Some(text) = term.selection_to_string().filter(|t| !t.is_empty()) {
            super::copy_item(ui, "Copy", &text);
        }
        // the terminal takes the keyboard first, so the paste reaches the shell as typed text
        if super::MenuItem::new("Paste").show(ui).clicked() {
            response.request_focus();
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            ui.close();
        }
    });
}

fn copy<L: EventListener>(ui: &Ui, term: &Term<L>) {
    if let Some(text) = term.selection_to_string() {
        ui.ctx().copy_text(text);
    }
}

fn paste(text: &str, mode: TermMode, input: &mut Vec<u8>) {
    if mode.contains(TermMode::BRACKETED_PASTE) {
        input.extend_from_slice(b"\x1b[200~");
        input.extend(text.bytes().filter(|&b| b != 0x1b));
        input.extend_from_slice(b"\x1b[201~");
    } else {
        input.extend(text.replace("\r\n", "\r").replace('\n', "\r").bytes());
    }
}

fn key_bytes(key: Key, m: Modifiers, mode: TermMode) -> Option<Cow<'static, [u8]>> {
    let modifier = 1 + m.shift as u8 + 2 * m.alt as u8 + 4 * m.ctrl as u8;
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let cursor = |c: char| -> Cow<'static, [u8]> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{c}").into_bytes().into()
        } else if app_cursor {
            format!("\x1bO{c}").into_bytes().into()
        } else {
            format!("\x1b[{c}").into_bytes().into()
        }
    };
    let tilde = |n: u8| -> Cow<'static, [u8]> {
        if modifier > 1 {
            format!("\x1b[{n};{modifier}~").into_bytes().into()
        } else {
            format!("\x1b[{n}~").into_bytes().into()
        }
    };
    let alt = |bytes: &'static [u8]| -> Cow<'static, [u8]> {
        if m.alt {
            [b"\x1b", bytes].concat().into()
        } else {
            bytes.into()
        }
    };

    Some(match key {
        Key::Enter => alt(b"\r"),
        Key::Backspace if m.ctrl => alt(b"\x08"),
        Key::Backspace => alt(b"\x7f"),
        Key::Tab if m.shift => b"\x1b[Z".as_slice().into(),
        Key::Tab => b"\t".as_slice().into(),
        Key::Escape => b"\x1b".as_slice().into(),
        Key::ArrowUp => cursor('A'),
        Key::ArrowDown => cursor('B'),
        Key::ArrowRight => cursor('C'),
        Key::ArrowLeft => cursor('D'),
        Key::Home => cursor('H'),
        Key::End => cursor('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F1 => b"\x1bOP".as_slice().into(),
        Key::F2 => b"\x1bOQ".as_slice().into(),
        Key::F3 => b"\x1bOR".as_slice().into(),
        Key::F4 => b"\x1bOS".as_slice().into(),
        Key::F5 => tilde(15),
        Key::F6 => tilde(17),
        Key::F7 => tilde(18),
        Key::F8 => tilde(19),
        Key::F9 => tilde(20),
        Key::F10 => tilde(21),
        Key::F11 => tilde(23),
        Key::F12 => tilde(24),
        _ if m.ctrl => {
            let byte = match key {
                Key::Space => 0,
                Key::OpenBracket => 0x1b,
                Key::Backslash => 0x1c,
                Key::CloseBracket => 0x1d,
                _ => {
                    let name = key.name().as_bytes();
                    match name {
                        [c] if c.is_ascii_alphabetic() => c.to_ascii_lowercase() - b'a' + 1,
                        _ => return None,
                    }
                }
            };
            if m.alt {
                vec![0x1b, byte].into()
            } else {
                vec![byte].into()
            }
        }
        _ => return None,
    })
}

struct Run {
    line: usize,
    col: usize,
    text: String,
    fg: Color32,
    bold: bool,
    italic: bool,
}

fn paint<L: EventListener>(
    ui: &Ui,
    p: &Palette,
    term: &Term<L>,
    grid: &Grid,
    cursor_state: Cursor,
    fonts: &Fonts,
) {
    let painter = ui.painter();
    let content = term.renderable_content();
    let colors = content.colors;
    let offset = content.display_offset;
    let selection = content.selection;
    let default_bg = resolve(Color::Named(NamedColor::Background), colors, p);

    let mut run: Option<Run> = None;
    let flush = |run: Option<Run>| {
        if let Some(run) = run
            && !run.text.trim().is_empty()
        {
            painter.text(
                grid.cell_rect(run.line, run.col, 1).min,
                Align2::LEFT_TOP,
                run.text,
                fonts.pick(run.bold, run.italic).clone(),
                run.fg,
            );
        }
    };

    for indexed in content.display_iter {
        let cell = indexed.cell;
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let Some(vp) = point_to_viewport(offset, indexed.point) else {
            continue;
        };
        let (line, col) = (vp.line, vp.column.0);

        let mut fg = resolve(cell.fg, colors, p);
        let mut bg = resolve(cell.bg, colors, p);
        if cell.flags.contains(Flags::DIM) {
            fg = fg.gamma_multiply(0.66);
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if selection.is_some_and(|s| s.contains(indexed.point)) {
            bg = p.accent.gamma_multiply(0.35);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }

        let width = if cell.flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let rect = grid.cell_rect(line, col, width);
        if bg != default_bg {
            // nudge by a pixel so neighbouring cells overlap instead of leaving hairline gaps
            painter.rect_filled(rect.expand2(vec2(0.5, 0.0)), 0.0, bg);
        }
        if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            painter.hline(rect.x_range(), rect.bottom() - 1.5, Stroke::new(1.0, fg));
        }
        if cell.flags.contains(Flags::STRIKEOUT) {
            painter.hline(rect.x_range(), rect.center().y, Stroke::new(1.0, fg));
        }

        let bold = cell.flags.contains(Flags::BOLD);
        let italic = cell.flags.contains(Flags::ITALIC);
        // only plain ascii shares the font's advance, so anything else gets placed on its own cell
        let simple = cell.c.is_ascii() && width == 1;
        let extends = run.as_ref().is_some_and(|r| {
            simple
                && r.line == line
                && r.col + r.text.len() == col
                && r.fg == fg
                && r.bold == bold
                && r.italic == italic
        });
        if extends {
            if let Some(r) = &mut run {
                r.text.push(cell.c);
            }
        } else {
            flush(run.take());
            run = Some(Run {
                line,
                col,
                text: cell.c.to_string(),
                fg,
                bold,
                italic,
            });
            if !simple {
                flush(run.take());
            }
        }
    }
    flush(run);

    let cursor = content.cursor;
    if cursor.shape == CursorShape::Hidden || cursor_state == Cursor::BlinkedOff {
        return;
    }
    let Some(vp) = point_to_viewport(offset, cursor.point) else {
        return;
    };
    let color = resolve(Color::Named(NamedColor::Cursor), colors, p);
    let cell = &term.grid()[cursor.point];
    let width = if cell.flags.contains(Flags::WIDE_CHAR) {
        2
    } else {
        1
    };
    let rect = grid.cell_rect(vp.line, vp.column.0, width);
    match (cursor.shape, cursor_state == Cursor::Visible) {
        (CursorShape::Block, true) => {
            painter.rect_filled(rect, 1.0, color);
            if cell.c != ' ' {
                painter.text(
                    rect.min,
                    Align2::LEFT_TOP,
                    cell.c,
                    fonts.regular.clone(),
                    default_bg,
                );
            }
        }
        (CursorShape::Beam, _) => {
            painter.rect_filled(
                Rect::from_min_size(rect.min, vec2(2.0, rect.height())),
                0.0,
                color,
            );
        }
        (CursorShape::Underline, _) => {
            painter.rect_filled(
                Rect::from_min_max(pos2(rect.left(), rect.bottom() - 2.0), rect.max),
                0.0,
                color,
            );
        }
        _ => {
            painter.rect_stroke(rect, 1.0, Stroke::new(1.0, color), StrokeKind::Inside);
        }
    }
}

fn resolve(color: Color, colors: &Colors, p: &Palette) -> Color32 {
    let rgb = |c: Rgb| Color32::from_rgb(c.r, c.g, c.b);
    match color {
        Color::Spec(c) => rgb(c),
        Color::Named(n) => colors[n].map(rgb).unwrap_or_else(|| named(n, p)),
        Color::Indexed(i) => colors[i as usize].map(rgb).unwrap_or_else(|| indexed(i, p)),
    }
}

fn named(n: NamedColor, p: &Palette) -> Color32 {
    let i = n as usize;
    match n {
        _ if i < 16 => p.ansi[i],
        NamedColor::Background => p.bg,
        NamedColor::DimForeground => p.text_muted,
        NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => p.text,
        _ => p.ansi[i - NamedColor::DimBlack as usize].gamma_multiply(0.7),
    }
}

// the standard xterm 256 color layout: 16 theme colors, a 6x6x6 cube, then a gray ramp
fn indexed(i: u8, p: &Palette) -> Color32 {
    match i {
        0..16 => p.ansi[i as usize],
        16..232 => {
            let i = i - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            Color32::from_rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
        }
        _ => {
            let v = 8 + 10 * (i - 232);
            Color32::from_rgb(v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(key: Key, m: Modifiers, mode: TermMode) -> Vec<u8> {
        key_bytes(key, m, mode).unwrap().into_owned()
    }

    #[test]
    fn keys() {
        let none = TermMode::empty();
        assert_eq!(key(Key::ArrowUp, Modifiers::NONE, none), b"\x1b[A");
        assert_eq!(
            key(Key::ArrowUp, Modifiers::NONE, TermMode::APP_CURSOR),
            b"\x1bOA"
        );
        assert_eq!(key(Key::ArrowLeft, Modifiers::CTRL, none), b"\x1b[1;5D");
        assert_eq!(key(Key::C, Modifiers::CTRL, none), [0x03]);
        assert_eq!(key(Key::Delete, Modifiers::NONE, none), b"\x1b[3~");
        assert_eq!(key(Key::Tab, Modifiers::SHIFT, none), b"\x1b[Z");
        assert!(key_bytes(Key::A, Modifiers::NONE, none).is_none());
    }

    #[test]
    fn mouse_reports() {
        let mut out = Vec::new();
        report(&mut out, TermMode::SGR_MOUSE, 0, (4, 9), true);
        report(&mut out, TermMode::SGR_MOUSE, 0, (4, 9), false);
        assert_eq!(out, b"\x1b[<0;10;5M\x1b[<0;10;5m");

        let mut out = Vec::new();
        report(&mut out, TermMode::MOUSE_REPORT_CLICK, 2, (0, 0), false);
        assert_eq!(out, [0x1b, b'[', b'M', 32 + 3, 33, 33]);
    }
}
