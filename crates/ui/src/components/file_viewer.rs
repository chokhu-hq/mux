use std::path::Path;
use std::sync::{Arc, Mutex};

use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CornerRadius, Galley, Rect, ScrollArea, Sense, Stroke, Ui, UiBuilder, pos2,
    vec2,
};

use super::{IconButton, TooltipExt};
use crate::icons;
use crate::theme::{self, Palette, font, icon, space, text};

// big enough for any source file, small enough to highlight without a stall
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const HEADER: f32 = 44.0;

pub enum FileViewAction {
    Close,
    ToggleWrap,
    CopyPath,
    OpenInEditor,
    // move to the previous or next changed file
    Step(isize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Context,
    Add,
    Remove,
    // the `@@` line that starts each hunk
    Hunk,
}

// one line of a diff and where it sits in the old and new file
struct DiffRow {
    kind: RowKind,
    old: Option<usize>,
    new: Option<usize>,
}

struct Diff {
    rows: Vec<DiffRow>,
    added: usize,
    removed: usize,
}

// a read only look at one file, opened from a link in the chat
pub struct FileView {
    pub path: String,
    // the linked lines, counted from 1 and inclusive
    pub lines: Option<(usize, usize)>,
    content: Result<Arc<str>, String>,
    // the coloured file from a background thread, tagged with the theme it was coloured for
    colored: Arc<Mutex<Option<(Palette, LayoutJob)>>>,
    // the theme a colouring thread was last started for
    coloring: Option<Palette>,
    pub wrap: bool,
    // the laid out file and what it was laid out for: theme, coloured or plain, and wrap width
    galley: Option<((Palette, bool, u32), Arc<Galley>)>,
    // the row each line starts on, since a wrapped line takes several rows
    line_rows: Vec<usize>,
    // frames left to keep pinning the scroll to the linked lines, since the first frame doesn't know the height yet
    settle: u8,
    // set when showing a diff rather than the file, one row per line of content
    diff: Option<Diff>,
    // which of the changed files this is, and how many there are
    pub step: Option<(usize, usize)>,
}

impl FileView {
    pub fn open(path: String, lines: Option<(usize, usize)>) -> Self {
        let content = read(&path).map(Arc::from);
        // prose reads better wrapped, code keeps its own line breaks
        let ext = extension(&path);
        let wrap = matches!(ext.as_str(), "md" | "markdown" | "txt" | "log" | "");
        Self {
            path,
            lines,
            content,
            colored: Arc::default(),
            coloring: None,
            wrap,
            galley: None,
            line_rows: Vec::new(),
            settle: 3,
            diff: None,
            step: None,
        }
    }

    // a file's changes from a unified diff, scrolled to the first one
    pub fn diff(path: String, unified: &str) -> Self {
        let (text, diff) = parse_diff(unified);
        let first = diff
            .rows
            .iter()
            .position(|r| matches!(r.kind, RowKind::Add | RowKind::Remove))
            .map(|i| i + 1);
        let content = if diff.rows.is_empty() {
            Err("No changes since the last commit".to_owned())
        } else {
            Ok(Arc::from(text))
        };
        Self {
            content,
            lines: first.map(|line| (line, line)),
            diff: Some(diff),
            ..Self::open(path, None)
        }
    }

    // the line in the file itself to open an editor at
    pub fn editor_line(&self) -> Option<usize> {
        match &self.diff {
            Some(diff) => diff
                .rows
                .iter()
                .find(|r| matches!(r.kind, RowKind::Add | RowKind::Remove))
                .and_then(|r| r.new.or(r.old)),
            None => self.lines.map(|(start, _)| start),
        }
    }

    pub fn show(&mut self, ui: &mut Ui) -> Option<FileViewAction> {
        let p = theme::palette(ui);
        let rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, p.bg);
        ui.painter().vline(
            rect.left() + 0.5,
            rect.y_range(),
            Stroke::new(1.0, p.border),
        );

        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER));
        let action = self.header(ui, header.shrink2(vec2(space::MD, 0.0)));
        ui.painter().hline(
            header.x_range(),
            header.bottom() - 0.5,
            Stroke::new(1.0, p.border),
        );

        let body = Rect::from_min_max(pos2(rect.left() + 1.0, header.bottom()), rect.max);
        let mut body_ui = ui.new_child(UiBuilder::new().id_salt("body").max_rect(body));
        body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
        match &self.content {
            Ok(_) => self.code(&mut body_ui),
            Err(reason) => {
                body_ui.painter().text(
                    body.center(),
                    Align2::CENTER_CENTER,
                    reason,
                    font::regular(text::MD),
                    p.text_muted,
                );
            }
        }
        action
    }

    fn header(&self, ui: &mut Ui, rect: Rect) -> Option<FileViewAction> {
        let p = theme::palette(ui);
        let mut action = None;
        let mut row = ui.new_child(
            UiBuilder::new()
                .id_salt("header")
                .max_rect(rect)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        row.spacing_mut().item_spacing.x = space::XS;
        if IconButton::new(icons::X, "Close")
            .show(&mut row)
            .tip("Close")
            .clicked()
        {
            action = Some(FileViewAction::Close);
        }
        if IconButton::new(icons::ARROW_SQUARE_OUT, "Open in editor")
            .show(&mut row)
            .tip("Open in editor")
            .clicked()
        {
            action = Some(FileViewAction::OpenInEditor);
        }
        if IconButton::new(icons::ARROW_U_DOWN_LEFT, "Wrap lines")
            .selected(self.wrap)
            .show(&mut row)
            .tip(if self.wrap {
                "Stop wrapping lines"
            } else {
                "Wrap long lines"
            })
            .clicked()
        {
            action = Some(FileViewAction::ToggleWrap);
        }
        if IconButton::new(icons::COPY, "Copy path")
            .show(&mut row)
            .tip("Copy path")
            .clicked()
        {
            action = Some(FileViewAction::CopyPath);
        }
        if let Some((index, count)) = self.step.filter(|(_, count)| *count > 1) {
            if IconButton::new(icons::CARET_RIGHT, "Next file")
                .show(&mut row)
                .tip("Next changed file")
                .clicked()
            {
                action = Some(FileViewAction::Step(1));
            }
            row.label(
                egui::RichText::new(format!("{} of {count}", index + 1))
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_faint),
            );
            if IconButton::new(icons::CARET_LEFT, "Previous file")
                .show(&mut row)
                .tip("Previous changed file")
                .clicked()
            {
                action = Some(FileViewAction::Step(-1));
            }
        }
        let buttons_left = row.min_rect().left();

        let file = Path::new(&self.path);
        let name = file
            .file_name()
            .map_or_else(|| self.path.clone(), |n| n.to_string_lossy().into_owned());
        let folder = file
            .parent()
            .map(|d| mux_core::display_path(&d.to_string_lossy()))
            .unwrap_or_default();
        let icon_center = pos2(rect.left() + icon::MD / 2.0, rect.center().y);
        ui.painter().text(
            icon_center,
            Align2::CENTER_CENTER,
            icons::FILE,
            font::icon(icon::MD),
            p.text_muted,
        );
        let name_galley = ui
            .painter()
            .layout_no_wrap(name, font::medium(text::MD), p.text);
        let name_pos = pos2(
            icon_center.x + icon::MD / 2.0 + space::SM,
            rect.center().y - name_galley.size().y / 2.0,
        );
        let mut name_right = name_pos.x + name_galley.size().x;
        ui.painter().galley(name_pos, name_galley, p.text);
        if let Some(diff) = &self.diff {
            let plus = ui.painter().text(
                pos2(name_right + space::SM, rect.center().y),
                Align2::LEFT_CENTER,
                format!("+{}", diff.added),
                font::medium(text::SM + 1.0),
                p.success,
            );
            let minus = ui.painter().text(
                pos2(plus.right() + space::XS, rect.center().y),
                Align2::LEFT_CENTER,
                format!("−{}", diff.removed),
                font::medium(text::SM + 1.0),
                p.danger,
            );
            name_right = minus.right();
        }
        let room = buttons_left - space::SM - name_right - space::SM;
        if room > 24.0 {
            let mut job = egui::text::LayoutJob::simple_singleline(
                folder,
                font::regular(text::SM + 1.0),
                p.text_faint,
            );
            job.wrap = egui::text::TextWrapping {
                max_width: room,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = ui.painter().layout_job(job);
            ui.painter().galley(
                pos2(
                    name_right + space::SM,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                p.text_faint,
            );
        }
        action
    }

    fn code(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui);
        let size = text::MONO_SM;
        let dark = ui.visuals().dark_mode;
        let palette = *p;
        let Ok(content) = &self.content else {
            return;
        };
        let content = content.clone();
        if self.coloring != Some(palette) {
            self.coloring = Some(palette);
            let (slot, ctx, text, ext) = (
                self.colored.clone(),
                ui.ctx().clone(),
                content.clone(),
                extension(&self.path),
            );
            // colouring a whole file takes long enough to drop frames, so it happens off the ui thread
            std::thread::spawn(move || {
                if let Some(job) =
                    crate::highlight::colorize(&text, &ext, &palette, dark, font::mono(size))
                {
                    if let Ok(mut slot) = slot.lock() {
                        *slot = Some((palette, job));
                    }
                    ctx.request_repaint();
                }
            });
        }

        let lines = content.lines().count().max(1);
        let digit = ui
            .painter()
            .layout_no_wrap("0".to_owned(), font::mono(size), p.text_faint)
            .size()
            .x;
        // a diff numbers both sides, the old file's line then the new one's, with room for the sign
        let biggest = self.diff.as_ref().map_or(lines, |d| {
            d.rows
                .iter()
                .filter_map(|r| r.old.max(r.new))
                .max()
                .unwrap_or(1)
        });
        let column = biggest.to_string().len() as f32 * digit;
        let gutter = if self.diff.is_some() {
            column * 2.0 + space::MD * 2.0 + digit + space::LG
        } else {
            column + space::LG * 2.0
        };
        let pad = space::SM;
        let width = if self.wrap {
            (ui.available_width() - gutter - space::LG).max(120.0)
        } else {
            f32::INFINITY
        };
        let colored = self.colored.lock().ok().and_then(|slot| {
            slot.clone()
                .filter(|(colored_for, _)| *colored_for == palette)
                .map(|(_, job)| job)
        });
        let key = (palette, colored.is_some(), width.to_bits());
        let galley = match &self.galley {
            Some((laid, galley)) if *laid == key => galley.clone(),
            _ => {
                let mut job = colored.unwrap_or_else(|| {
                    LayoutJob::simple(content.to_string(), font::mono(size), p.text, f32::INFINITY)
                });
                if let Some(diff) = &self.diff {
                    mute_hunks(&mut job, &content, diff, p.text_faint);
                }
                job.wrap.max_width = width;
                let galley = ui.painter().layout_job(job);
                self.line_rows = line_rows(&galley);
                self.galley = Some((key, galley.clone()));
                galley
            }
        };
        let line_rows = self.line_rows.clone();
        let row_of = |line: usize| line_rows.get(line.saturating_sub(1)).copied();

        let target = self
            .lines
            .and_then(|(start, _)| galley.rows.get(row_of(start)?).map(|row| row.min_y()));
        let mut area = ScrollArea::both()
            .id_salt(("file", &self.path))
            .auto_shrink(false);
        // hidden until the scroll has settled on the linked lines, then faded in, so it never jumps
        let fade_id = ui.scope_id().with("fade");
        if self.settle > 0 {
            ui.ctx().request_repaint();
            ui.set_opacity(0.0);
            // read before taking the data lock, since both lock the same context
            let now = ui.input(|i| i.time);
            ui.data_mut(|d| d.insert_temp(fade_id, now));
        } else if let Some(since) = ui.data(|d| d.get_temp::<f64>(fade_id)) {
            let t = ((ui.input(|i| i.time) - since) / 0.12).clamp(0.0, 1.0) as f32;
            if t < 1.0 {
                ui.ctx().request_repaint();
            }
            ui.set_opacity(t);
        }
        if self.settle > 0 {
            self.settle -= 1;
            // the linked line sits a little below the top, so what leads into it stays visible
            if let Some(y) = target {
                area = area
                    .vertical_scroll_offset((y - 80.0).max(0.0))
                    .horizontal_scroll_offset(0.0);
            }
        }
        area.show(ui, |ui| {
            let total = vec2(
                gutter + galley.size().x + space::LG,
                galley.size().y + pad * 2.0,
            );
            let (rect, _) = ui.allocate_exact_size(total.max(ui.available_size()), Sense::hover());
            let origin = pos2(rect.left() + gutter, rect.top() + pad);
            let visible = ui.clip_rect();

            if let Some(diff) = &self.diff {
                paint_diff_rows(ui, &galley, &line_rows, diff, origin.y, visible);
            } else if let Some((start, end)) = self.lines {
                let rows = &galley.rows;
                // the band runs to the row before the line after the range, so wrapped lines are covered whole
                let last_row = row_of(end + 1).unwrap_or(rows.len()).saturating_sub(1);
                if let (Some(first), Some(last)) = (
                    row_of(start).and_then(|r| rows.get(r)),
                    rows.get(last_row).or(rows.last()),
                ) {
                    let band = Rect::from_min_max(
                        pos2(visible.left(), origin.y + first.min_y()),
                        pos2(visible.right(), origin.y + last.max_y()),
                    );
                    ui.painter().rect_filled(
                        band,
                        CornerRadius::ZERO,
                        p.accent.gamma_multiply(0.12),
                    );
                    ui.painter().vline(
                        visible.left() + 1.0,
                        band.y_range(),
                        Stroke::new(2.0, p.accent),
                    );
                }
            }

            // numbers are drawn only for lines on screen, each on the first row of its line
            for (i, &first) in line_rows.iter().enumerate() {
                let Some(row) = galley.rows.get(first) else {
                    continue;
                };
                let top = origin.y + row.min_y();
                let bottom = origin.y + row.max_y();
                if bottom < visible.top() {
                    continue;
                }
                if top > visible.bottom() {
                    break;
                }
                let line = i + 1;
                let mid = (top + bottom) / 2.0;
                let left = visible.left().max(rect.left());
                if let Some(diff) = &self.diff {
                    let Some(row) = diff.rows.get(i) else {
                        continue;
                    };
                    let number = |n: Option<usize>| n.map(|n| n.to_string()).unwrap_or_default();
                    let mono = font::mono(size);
                    ui.painter().text(
                        pos2(left + space::MD + column, mid),
                        Align2::RIGHT_CENTER,
                        number(row.old),
                        mono.clone(),
                        p.text_faint,
                    );
                    ui.painter().text(
                        pos2(left + space::MD * 2.0 + column * 2.0, mid),
                        Align2::RIGHT_CENTER,
                        number(row.new),
                        mono.clone(),
                        p.text_faint,
                    );
                    let (sign, color) = match row.kind {
                        RowKind::Add => ("+", p.success),
                        RowKind::Remove => ("−", p.danger),
                        _ => ("", p.text_faint),
                    };
                    ui.painter().text(
                        pos2(left + gutter - space::SM, mid),
                        Align2::RIGHT_CENTER,
                        sign,
                        mono,
                        color,
                    );
                    continue;
                }
                let linked = self
                    .lines
                    .is_some_and(|(start, end)| (start..=end).contains(&line));
                ui.painter().text(
                    pos2(
                        visible.left().max(rect.left()) + gutter - space::LG,
                        (top + bottom) / 2.0,
                    ),
                    Align2::RIGHT_CENTER,
                    line.to_string(),
                    font::mono(size),
                    if linked { p.text } else { p.text_faint },
                );
            }
            // a label rather than painted text, so the code can be selected and copied
            ui.put(
                Rect::from_min_size(origin, galley.size()),
                egui::Label::new(galley.clone()).selectable(true),
            );
        });
    }
}

// added and removed lines get a wash of green or red across the whole row, hunk starts a quiet grey
fn paint_diff_rows(
    ui: &Ui,
    galley: &Galley,
    line_rows: &[usize],
    diff: &Diff,
    top: f32,
    visible: Rect,
) {
    let p = theme::palette(ui);
    for (i, row) in diff.rows.iter().enumerate() {
        let fill = match row.kind {
            RowKind::Add => p.success.gamma_multiply(0.12),
            RowKind::Remove => p.danger.gamma_multiply(0.12),
            RowKind::Hunk => theme::tint(p, theme::fill::HOVER),
            RowKind::Context => continue,
        };
        let (Some(&first), last) = (line_rows.get(i), line_rows.get(i + 1)) else {
            continue;
        };
        let last = last
            .map_or(galley.rows.len(), |&next| next)
            .saturating_sub(1);
        let (Some(a), Some(b)) = (galley.rows.get(first), galley.rows.get(last)) else {
            continue;
        };
        let band = Rect::from_min_max(
            pos2(visible.left(), top + a.min_y()),
            pos2(visible.right(), top + b.max_y()),
        );
        if band.max.y < visible.min.y || band.min.y > visible.max.y {
            continue;
        }
        ui.painter().rect_filled(band, CornerRadius::ZERO, fill);
    }
}

// syntax colours make no sense on a hunk's `@@` line, so it's greyed out over them
fn mute_hunks(job: &mut LayoutJob, text: &str, diff: &Diff, color: Color32) {
    let mut start = 0;
    let mut ranges = Vec::new();
    for (line, row) in text.split_inclusive('\n').zip(&diff.rows) {
        if row.kind == RowKind::Hunk {
            ranges.push(start..start + line.len());
        }
        start += line.len();
    }
    for section in &mut job.sections {
        if ranges
            .iter()
            .any(|r| r.contains(&section.byte_range.start.0))
        {
            section.format.color = color;
        }
    }
}

// the lines of a unified diff without their signs, and what each one was
fn parse_diff(unified: &str) -> (String, Diff) {
    let mut text = String::new();
    let mut rows = Vec::new();
    let (mut old, mut new) = (0, 0);
    let mut started = false;
    for line in unified.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            let numbers = |sign: char| {
                rest.split_whitespace()
                    .find_map(|part| part.strip_prefix(sign))
                    .and_then(|part| part.split(',').next()?.parse::<usize>().ok())
                    .unwrap_or(1)
            };
            (old, new) = (numbers('-'), numbers('+'));
            started = true;
            // the function or heading git found for the hunk reads better than the numbers
            let context = rest.split("@@").nth(1).unwrap_or_default().trim();
            text.push_str(if context.is_empty() { "⋯" } else { context });
            text.push('\n');
            rows.push(DiffRow {
                kind: RowKind::Hunk,
                old: None,
                new: None,
            });
            continue;
        }
        if !started || line.starts_with('\\') {
            continue;
        }
        let (kind, body) = match line.chars().next() {
            Some('+') => (RowKind::Add, &line[1..]),
            Some('-') => (RowKind::Remove, &line[1..]),
            Some(' ') => (RowKind::Context, &line[1..]),
            None => (RowKind::Context, ""),
            _ => continue,
        };
        rows.push(DiffRow {
            kind,
            old: (kind != RowKind::Add).then_some(old),
            new: (kind != RowKind::Remove).then_some(new),
        });
        if kind != RowKind::Add {
            old += 1;
        }
        if kind != RowKind::Remove {
            new += 1;
        }
        text.push_str(body);
        text.push('\n');
    }
    text.pop();
    let count = |kind| rows.iter().filter(|r| r.kind == kind).count();
    let (added, removed) = (count(RowKind::Add), count(RowKind::Remove));
    (
        text,
        Diff {
            rows,
            added,
            removed,
        },
    )
}

fn extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

// the row index where each line begins, found from the rows that close a line
fn line_rows(galley: &Galley) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, row) in galley.rows.iter().enumerate() {
        if row.ends_with_newline {
            starts.push(i + 1);
        }
    }
    starts
}

fn read(path: &str) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Couldn't open this file: {e}"))?;
    if meta.is_dir() {
        return Err("This is a folder, not a file.".to_owned());
    }
    if meta.len() > MAX_BYTES {
        return Err("This file is too large to show here.".to_owned());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read this file: {e}"))?;
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err("This looks like a binary file.".to_owned());
    }
    String::from_utf8(bytes).map_err(|_| "This file isn't text.".to_owned())
}

// where a chat link points: a file and the lines it names, with relative paths taken from the project folder
pub fn link_target(target: &str, base: Option<&str>) -> Option<(String, Option<(usize, usize)>)> {
    if target.contains("://") && !target.starts_with("file://") {
        return None;
    }
    let target = target.strip_prefix("file://").unwrap_or(target);
    let (path, lines) = split_lines(target);
    let path = if path.starts_with('/') || Path::new(path).is_absolute() {
        path.to_owned()
    } else {
        mux_core::paths::join(base?, path)
    };
    Some((path, lines))
}

// handles path:12, path:12:5, path:12-20 and path#L12-L20
fn split_lines(target: &str) -> (&str, Option<(usize, usize)>) {
    let number = |s: &str| s.trim_start_matches('L').parse::<usize>().ok();
    let range = |s: &str| match s.split_once('-') {
        Some((a, b)) => Some((number(a)?, number(b)?)),
        None => number(s).map(|n| (n, n)),
    };
    if let Some((path, rest)) = target.split_once("#L")
        && let Some(lines) = range(rest)
    {
        return (path, Some(lines));
    }
    let mut path = target;
    let mut lines = None;
    // a trailing column is dropped, then the line or range before it is kept
    for _ in 0..2 {
        if let Some((head, tail)) = path.rsplit_once(':')
            && let Some(found) = range(tail)
        {
            lines = Some(found);
            path = head;
        } else {
            break;
        }
    }
    (path, lines)
}

// prefers an editor that can jump to the line, and falls back to the desktop's default app
pub fn open_in_editor(path: &str, line: Option<usize>) {
    let at = match line {
        Some(line) => format!("{path}:{line}"),
        None => path.to_owned(),
    };
    let on_path = |program: &str| {
        std::env::var_os("PATH")
            .is_some_and(|dirs| std::env::split_paths(&dirs).any(|d| d.join(program).is_file()))
    };
    let mut command = if on_path("zed") {
        let mut c = mux_core::command("zed");
        c.arg(&at);
        c
    } else if on_path("code") {
        let mut c = mux_core::command("code");
        c.args(["-g", &at]);
        c
    } else {
        let mut c = mux_core::command(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        });
        c.arg(path);
        c
    };
    let _ = command.spawn();
}

#[cfg(test)]
mod tests {
    use super::{RowKind, link_target, parse_diff, split_lines};

    #[test]
    fn reads_a_unified_diff_into_numbered_rows() {
        let unified = "diff --git a/src/auth.rs b/src/auth.rs\nindex 1..2 100644\n--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -10,3 +10,4 @@ pub fn auth()\n let a = 1;\n-let b = 2;\n+let b = 3;\n+let c = 4;\n let d = 5;\n\\ No newline at end of file\n";
        let (text, diff) = parse_diff(unified);
        assert_eq!(
            text,
            "pub fn auth()\nlet a = 1;\nlet b = 2;\nlet b = 3;\nlet c = 4;\nlet d = 5;"
        );
        let kinds: Vec<RowKind> = diff.rows.iter().map(|r| r.kind).collect();
        assert!(matches!(
            kinds.as_slice(),
            [
                RowKind::Hunk,
                RowKind::Context,
                RowKind::Remove,
                RowKind::Add,
                RowKind::Add,
                RowKind::Context
            ]
        ));
        let numbers: Vec<_> = diff.rows.iter().map(|r| (r.old, r.new)).collect();
        assert_eq!(
            numbers,
            [
                (None, None),
                (Some(10), Some(10)),
                (Some(11), None),
                (None, Some(11)),
                (None, Some(12)),
                (Some(12), Some(13))
            ]
        );
        assert_eq!((diff.added, diff.removed), (2, 1));
    }

    #[test]
    fn reads_lines_from_link_targets() {
        assert_eq!(split_lines("/a/b.rs:127"), ("/a/b.rs", Some((127, 127))));
        assert_eq!(split_lines("/a/b.rs:12:5"), ("/a/b.rs", Some((12, 12))));
        assert_eq!(split_lines("/a/b.rs:12-20"), ("/a/b.rs", Some((12, 20))));
        assert_eq!(split_lines("/a/b.rs#L3-L9"), ("/a/b.rs", Some((3, 9))));
        assert_eq!(split_lines("/a/b.rs"), ("/a/b.rs", None));
    }

    #[test]
    fn resolves_relative_links_against_the_project() {
        assert_eq!(
            link_target("src/main.rs:4", Some("/p")),
            Some(("/p/src/main.rs".to_owned(), Some((4, 4))))
        );
        assert_eq!(link_target("https://example.com", Some("/p")), None);
        assert_eq!(link_target("src/main.rs", None), None);
    }
}
