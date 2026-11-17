use std::path::Path;
use std::sync::{Arc, Mutex};

use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Event, Frame, Galley, Key, Margin, Modifiers, Rect,
    ScrollArea, Sense, Stroke, TextEdit, Ui, UiBuilder, pos2, vec2,
};

use super::{Button, Chip, DiffComment, IconButton, MenuItem, Size, TooltipExt, hover_t};
use crate::icons;
use crate::theme::{self, Palette, font, icon, radius, space, text};

// big enough for any source file, small enough to highlight without a stall
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const HEADER: f32 = 44.0;
// the changed files column, and how wide the panel has to be before there's room for it
const COLUMN: f32 = 220.0;
const COLUMN_AT: f32 = 720.0;
const FILE_ROW: f32 = 28.0;
// room left under the last line for a comment field opened there
const FIELD_ROOM: f32 = 140.0;

pub enum FileViewAction {
    Close,
    ToggleWrap,
    CopyPath,
    OpenInEditor,
    // move to the previous or next changed file
    Step(isize),
    // ask the chat's agent to commit what it changed
    Commit,
    // push the worktree's branch and open a pull request for it
    Push,
    // open this one of the changed files
    Open(usize),
    Scope(DiffScope),
    Comment(DiffComment),
}

// which changes the diff view compares against
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DiffScope {
    // the working tree against the last commit
    #[default]
    Uncommitted,
    // the working tree against where the branch left the default branch
    Branch,
    // only the hunks the agent showed in the chat
    Agent,
}

impl DiffScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Uncommitted => "Uncommitted",
            Self::Branch => "Branch",
            Self::Agent => "Agent",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ChangedFile {
    pub path: String,
    // the path as listed, relative to the repository
    pub name: String,
    pub added: usize,
    pub removed: usize,
}

// every changed file in the scope shown, listed beside the diff
#[derive(Clone, Debug, Default)]
pub struct DiffFiles {
    pub files: Vec<ChangedFile>,
    pub current: Option<usize>,
    pub scope: DiffScope,
    // what Branch compares against, offered only when there's one
    pub base: Option<String>,
    pub loading: bool,
}

// a comment being written on some lines of the diff, as rows of it
struct Draft {
    start: usize,
    end: usize,
    text: String,
    focus: bool,
}

// commit and push for a worktree chat's changes, shown above its diffs
#[derive(Default)]
pub struct GitBar {
    pub pushing: bool,
    pub error: Option<String>,
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
    pub git: Option<GitBar>,
    pub files: Option<DiffFiles>,
    // comments already left on this file, whose lines are marked
    pub comments: Vec<DiffComment>,
    // the rows a drag down the gutter has covered so far, where it started first
    selecting: Option<(usize, usize)>,
    draft: Option<Draft>,
}

impl FileView {
    pub fn open(path: String, lines: Option<(usize, usize)>) -> Self {
        Self {
            content: read(&path).map(Arc::from),
            ..Self::blank(path, lines)
        }
    }

    // a diff still being read, shown until it arrives
    pub fn pending(path: String) -> Self {
        Self {
            content: Err("Loading the diff…".to_owned()),
            diff: Some(parse_diff("").1),
            ..Self::blank(path, None)
        }
    }

    fn blank(path: String, lines: Option<(usize, usize)>) -> Self {
        // prose reads better wrapped, code keeps its own line breaks
        let ext = extension(&path);
        let wrap = matches!(ext.as_str(), "md" | "markdown" | "txt" | "log" | "");
        Self {
            path,
            lines,
            content: Err(String::new()),
            colored: Arc::default(),
            coloring: None,
            wrap,
            galley: None,
            line_rows: Vec::new(),
            settle: 3,
            diff: None,
            step: None,
            git: None,
            files: None,
            comments: Vec::new(),
            selecting: None,
            draft: None,
        }
    }

    // a file's changes from a unified diff, scrolled to the first one
    pub fn diff(path: String, unified: &str) -> Self {
        let (text, diff) = parse_diff(&mux_core::clip_lines(unified));
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
            ..Self::blank(path, None)
        }
    }

    // a comment is being written, so escape cancels it rather than closing the panel
    pub fn commenting(&self) -> bool {
        self.draft.is_some()
    }

    // the rows from `start` to `end` as a comment on this file
    fn comment(&self, start: usize, end: usize, text: &str) -> Option<DiffComment> {
        let (diff, Ok(content)) = (self.diff.as_ref()?, &self.content) else {
            return None;
        };
        let lines: Vec<&str> = content.split('\n').collect();
        let rows: Vec<(&DiffRow, &str)> = (start..=end)
            .filter_map(|i| Some((diff.rows.get(i)?, *lines.get(i)?)))
            .collect();
        let (range, old, new, hunk) = review_hunk(&rows)?;
        Some(DiffComment {
            path: self.path.clone(),
            file: self.path.clone(),
            range,
            old,
            new,
            text: text.trim().to_owned(),
            diff: hunk,
        })
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
        let mut rect = ui.max_rect();
        ui.painter().rect_filled(rect, 0.0, p.bg);
        ui.painter().vline(
            rect.left() + 0.5,
            rect.y_range(),
            Stroke::new(1.0, p.border),
        );
        // a narrow panel leaves the whole width to the diff, and the scope picker moves into its header
        let mut action = None;
        let column = self.files.is_some() && rect.width() >= COLUMN_AT;
        if let Some(files) = self.files.as_ref().filter(|_| column) {
            let list = Rect::from_min_size(rect.min, vec2(COLUMN, rect.height()));
            action = files_column(ui, list, files);
            rect.min.x = list.right();
            ui.painter().vline(
                rect.left() + 0.5,
                rect.y_range(),
                Stroke::new(1.0, p.border),
            );
        }

        let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER));
        let action = self
            .header(ui, header.shrink2(vec2(space::MD, 0.0)), !column)
            .or(action);
        ui.painter().hline(
            header.x_range(),
            header.bottom() - 0.5,
            Stroke::new(1.0, p.border),
        );

        let mut top = header.bottom();
        if let Some(error) = self.git.as_ref().and_then(|g| g.error.as_deref()) {
            let galley = ui.painter().layout(
                error.to_owned(),
                font::regular(text::SM + 1.0),
                p.danger,
                rect.width() - space::MD * 2.0,
            );
            let strip = Rect::from_min_size(
                pos2(rect.left(), top),
                vec2(rect.width(), galley.size().y + space::SM * 2.0),
            );
            ui.painter()
                .rect_filled(strip, 0.0, p.danger.gamma_multiply(0.12));
            ui.painter().galley(
                pos2(strip.left() + space::MD, strip.top() + space::SM),
                galley,
                p.danger,
            );
            top = strip.bottom();
        }
        let body = Rect::from_min_max(pos2(rect.left() + 1.0, top), rect.max);
        let mut body_ui = ui.new_child(UiBuilder::new().id_salt("body").max_rect(body));
        body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
        match &self.content {
            Ok(_) => {
                if let Some(comment) = self.code(&mut body_ui) {
                    return Some(FileViewAction::Comment(comment));
                }
            }
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

    fn header(&self, ui: &mut Ui, mut rect: Rect, picker: bool) -> Option<FileViewAction> {
        let p = theme::palette(ui);
        let mut action = None;
        if let Some(files) = self.files.as_ref().filter(|_| picker) {
            let mut left = ui.new_child(
                UiBuilder::new()
                    .id_salt("header-scope")
                    .max_rect(rect)
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            action = scope_picker(&mut left, files).map(FileViewAction::Scope);
            rect.min.x = left.min_rect().right() + space::SM;
        }
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
        if let Some(git) = &self.git {
            row.add_space(space::SM);
            let push = Button::new(if git.pushing { "Pushing…" } else { "Push" })
                .size(Size::Sm)
                .enabled(!git.pushing)
                .show(&mut row)
                .tip("Push this branch and open a pull request on GitHub");
            if push.clicked() {
                action = Some(FileViewAction::Push);
            }
            let commit = Button::new("Commit")
                .size(Size::Sm)
                .show(&mut row)
                .tip("Ask the agent to commit its changes");
            if commit.clicked() {
                action = Some(FileViewAction::Commit);
            }
        }
        let buttons_left = row.min_rect().left();

        let (name, folder) = mux_core::paths::name_and_folder(&self.path);
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
            let galley = crate::components::elided(
                ui,
                folder,
                font::regular(text::SM + 1.0),
                p.text_faint,
                room,
            );
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

    fn code(&mut self, ui: &mut Ui) -> Option<DiffComment> {
        let p = theme::palette(ui);
        let size = text::MONO_SM;
        let dark = ui.visuals().dark_mode;
        let palette = *p;
        let Ok(content) = &self.content else {
            return None;
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
        let field_id = ui.scope_id().with("comment-field");
        // a click, the end of a drag, and the rows on screen with where each sits, read inside the scroll area
        let mut clicked = None;
        let mut spans: Vec<(usize, f32, f32)> = Vec::new();
        let mut submit = false;
        let mut cancel = false;
        area.show(ui, |ui| {
            let room = if self.draft.is_some() {
                FIELD_ROOM
            } else {
                0.0
            };
            let total = vec2(
                gutter + galley.size().x + space::LG,
                galley.size().y + pad * 2.0 + room,
            );
            let (rect, _) = ui.allocate_exact_size(total.max(ui.available_size()), Sense::hover());
            let origin = pos2(rect.left() + gutter, rect.top() + pad);
            let visible = ui.clip_rect();

            if let Some(diff) = &self.diff {
                paint_diff_rows(ui, &galley, &line_rows, diff, origin.y, visible);
                let chosen = self
                    .selecting
                    .or(self.draft.as_ref().map(|d| (d.start, d.end)));
                if let Some((a, b)) = chosen {
                    let (a, b) = (a.min(b), a.max(b));
                    let rows = &galley.rows;
                    let first = line_rows.get(a).and_then(|&r| rows.get(r));
                    let last = line_rows
                        .get(b + 1)
                        .map_or(rows.len(), |&r| r)
                        .saturating_sub(1);
                    if let (Some(first), Some(last)) = (first, rows.get(last)) {
                        let band = Rect::from_min_max(
                            pos2(visible.left(), origin.y + first.min_y()),
                            pos2(visible.right(), origin.y + last.max_y()),
                        );
                        ui.painter().rect_filled(
                            band,
                            CornerRadius::ZERO,
                            p.accent.gamma_multiply(0.16),
                        );
                    }
                }
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
                    // the line's whole height, wrapped rows and all, picks it for a comment
                    let next = line_rows
                        .get(i + 1)
                        .map_or(galley.rows.len(), |&r| r)
                        .saturating_sub(1);
                    let line_bottom = galley
                        .rows
                        .get(next)
                        .map_or(bottom, |r| origin.y + r.max_y());
                    let numbers =
                        Rect::from_min_max(pos2(left, top), pos2(left + gutter, line_bottom));
                    if self.comments.iter().any(|c| c.covers(row.old, row.new)) {
                        ui.painter().rect_filled(
                            numbers,
                            CornerRadius::ZERO,
                            p.accent.gamma_multiply(0.14),
                        );
                        ui.painter().vline(
                            left + 1.5,
                            numbers.y_range(),
                            Stroke::new(3.0, p.accent),
                        );
                    }
                    if row.kind != RowKind::Hunk {
                        spans.push((i, top, line_bottom));
                        let line = row.new.or(row.old).unwrap_or_default();
                        let label = match row.kind {
                            RowKind::Add => format!("Added line {line}"),
                            RowKind::Remove => format!("Removed line {line}"),
                            _ => format!("Line {line}"),
                        };
                        let response = ui
                            .interact(
                                numbers,
                                ui.scope_id().with(("gutter", i)),
                                Sense::click_and_drag(),
                            )
                            .on_hover_cursor(CursorIcon::PointingHand);
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Button, true, &label)
                        });
                        if response.hovered() && self.selecting.is_none() {
                            ui.painter().rect_filled(
                                numbers,
                                CornerRadius::ZERO,
                                theme::tint(p, theme::fill::HOVER),
                            );
                        }
                        if response.drag_started() {
                            self.selecting = Some((i, i));
                        }
                        if response.clicked() {
                            clicked = Some(i);
                        }
                    }
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
            // drawn after the code so it sits over the lines below the ones it's about
            if let Some(draft) = &mut self.draft {
                let rows = &galley.rows;
                let last = line_rows
                    .get(draft.end + 1)
                    .map_or(rows.len(), |&r| r)
                    .saturating_sub(1);
                let y = rows.get(last).map_or(origin.y, |r| origin.y + r.max_y()) + space::XS;
                let x = visible.left().max(rect.left()) + gutter;
                let width = (visible.right() - x - space::LG).clamp(160.0, 520.0);
                let mut field_ui = ui.new_child(
                    UiBuilder::new()
                        .id_salt("comment")
                        .max_rect(Rect::from_min_size(pos2(x, y), vec2(width, FIELD_ROOM))),
                );
                (submit, cancel) = comment_field(&mut field_ui, field_id, draft);
            }
        });
        // a drag down the gutter covers every line the pointer passes over
        if let Some((_, end)) = &mut self.selecting
            && let Some(y) = ui.input(|i| i.pointer.interact_pos()).map(|p| p.y)
            && let Some(&(i, _, _)) = spans
                .iter()
                .find(|(_, top, bottom)| (*top..*bottom).contains(&y))
                .or_else(|| spans.first().filter(|(_, top, _)| y < *top))
                .or_else(|| spans.last().filter(|(_, _, bottom)| y >= *bottom))
        {
            *end = i;
        }
        let shift = ui.input(|i| i.modifiers.shift);
        if let Some(i) = clicked {
            self.selecting = None;
            // shift extends from where the open comment starts, the way a shift click extends a selection
            let start = match &self.draft {
                Some(draft) if shift => draft.start,
                _ => i,
            };
            let text = self.draft.take().map(|d| d.text).unwrap_or_default();
            self.draft = Some(Draft {
                start: start.min(i),
                end: start.max(i),
                text,
                focus: true,
            });
        } else if !ui.input(|i| i.pointer.primary_down())
            && let Some((a, b)) = self.selecting.take()
        {
            let text = self.draft.take().map(|d| d.text).unwrap_or_default();
            self.draft = Some(Draft {
                start: a.min(b),
                end: a.max(b),
                text,
                focus: true,
            });
        }
        if cancel || (self.draft.is_some() && ui.input(|i| i.key_pressed(Key::Escape))) {
            self.draft = None;
            return None;
        }
        if submit && let Some(draft) = self.draft.take() {
            return self.comment(draft.start, draft.end, &draft.text);
        }
        None
    }
}

// the small field under commented lines, returning whether it was added or cancelled this frame
fn comment_field(ui: &mut Ui, id: egui::Id, draft: &mut Draft) -> (bool, bool) {
    let p = theme::palette(ui);
    let focused = ui.memory(|m| m.has_focus(id));
    // enter adds the comment and shift+enter starts a new line, as in the composer
    let mut submit = focused
        && ui.input_mut(|i| {
            let before = i.events.len();
            i.events.retain(|e| {
                !matches!(e, Event::Key { key: Key::Enter, pressed: true, modifiers, .. }
                    if !modifiers.shift && modifiers.matches_logically(Modifiers::NONE))
            });
            i.events.len() != before
        });
    let mut cancel = false;
    theme::floating(ui)
        .inner_margin(Margin::same(space::SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let field = ui.add(
                TextEdit::multiline(&mut draft.text)
                    .id(id)
                    .frame(Frame::NONE)
                    .hint_text("Comment for the agent")
                    .font(font::regular(text::MD))
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .margin(Margin::ZERO),
            );
            if std::mem::take(&mut draft.focus) {
                field.request_focus();
            }
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Enter to add, Esc to cancel")
                        .font(font::regular(text::SM))
                        .color(p.text_faint),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let ready = !draft.text.trim().is_empty();
                    submit |= Button::new("Comment")
                        .primary()
                        .size(Size::Sm)
                        .enabled(ready)
                        .show(ui)
                        .clicked();
                    cancel = Button::new("Cancel")
                        .ghost()
                        .size(Size::Sm)
                        .show(ui)
                        .clicked();
                });
            });
        });
    (submit && !draft.text.trim().is_empty(), cancel)
}

// which changes to show, as a chip that opens a menu
fn scope_picker(ui: &mut Ui, files: &DiffFiles) -> Option<DiffScope> {
    let mut picked = None;
    let chip = Chip::new(files.scope.label())
        .icon(icons::GIT_DIFF)
        .show(ui)
        .tip("Which changes to show");
    let branch = files
        .base
        .as_ref()
        .map(|base| format!("Everything since {base}"));
    egui::Popup::menu(&chip).gap(space::XS).show(|ui| {
        ui.set_width(260.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        let scopes = [
            (DiffScope::Uncommitted, "Changes since the last commit"),
            (DiffScope::Branch, branch.as_deref().unwrap_or_default()),
            (DiffScope::Agent, "What the agent showed in this chat"),
        ];
        for (scope, detail) in scopes {
            if scope == DiffScope::Branch && branch.is_none() {
                continue;
            }
            if MenuItem::new(scope.label())
                .detail(detail)
                .checked(files.scope == scope)
                .show(ui)
                .clicked()
            {
                picked = Some(scope).filter(|s| *s != files.scope);
                ui.close();
            }
        }
    });
    picked
}

// the changed files down the left of the diff, the open one lit, with the scope picker above them
fn files_column(ui: &mut Ui, rect: Rect, files: &DiffFiles) -> Option<FileViewAction> {
    let p = theme::palette(ui);
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER));
    let mut head = ui.new_child(
        UiBuilder::new()
            .id_salt("files-header")
            .max_rect(header.shrink2(vec2(space::SM, 0.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let mut action = scope_picker(&mut head, files).map(FileViewAction::Scope);
    if !files.files.is_empty() {
        head.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                egui::RichText::new(files.files.len().to_string())
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_faint),
            );
        });
    }
    ui.painter().hline(
        header.x_range(),
        header.bottom() - 0.5,
        Stroke::new(1.0, p.border),
    );
    let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
    let mut list = ui.new_child(UiBuilder::new().id_salt("files-list").max_rect(body));
    list.set_clip_rect(body.intersect(ui.clip_rect()));
    if files.files.is_empty() {
        let note = if files.loading {
            "Loading…"
        } else {
            "No changes"
        };
        list.painter().text(
            pos2(body.center().x, body.top() + space::XL),
            Align2::CENTER_TOP,
            note,
            font::regular(text::SM + 1.0),
            p.text_faint,
        );
        return action;
    }
    ScrollArea::vertical()
        .id_salt("changed-files")
        .auto_shrink(false)
        .show(&mut list, |ui| {
            ui.add_space(space::XS);
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, file) in files.files.iter().enumerate() {
                if file_row(ui, file, files.current == Some(i)) {
                    action = Some(FileViewAction::Open(i));
                }
            }
            ui.add_space(space::XS);
        });
    action
}

// one changed file: its name, its folder faint after it, and the lines added and removed
fn file_row(ui: &mut Ui, file: &ChangedFile, current: bool) -> bool {
    let p = theme::palette(ui);
    let (outer, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), FILE_ROW), Sense::click());
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, current, &file.name));
    let rect = outer.shrink2(vec2(space::XS, 1.0));
    let t = hover_t(ui, &response);
    let fill = if current {
        theme::tint(p, theme::fill::SELECTED)
    } else {
        p.hover.gamma_multiply(t)
    };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::MD), fill);
    let y = rect.center().y;
    let small = font::medium(text::SM);
    let minus = ui
        .painter()
        .layout_no_wrap(format!("−{}", file.removed), small.clone(), p.danger);
    let plus = ui
        .painter()
        .layout_no_wrap(format!("+{}", file.added), small, p.success);
    let mut right = rect.right() - space::SM;
    right -= minus.size().x;
    let minus_x = right;
    right -= space::XS + plus.size().x;
    let plus_x = right;
    ui.painter()
        .galley(pos2(minus_x, y - minus.size().y / 2.0), minus, p.danger);
    ui.painter()
        .galley(pos2(plus_x, y - plus.size().y / 2.0), plus, p.success);
    let (name, folder) = match file.name.rsplit_once('/') {
        Some((folder, name)) => (name, folder),
        None => (file.name.as_str(), ""),
    };
    let left = rect.left() + space::SM;
    let room = plus_x - space::SM - left;
    let name = super::elided(
        ui,
        name,
        font::regular(text::SM + 1.0),
        if current { p.text } else { p.text_muted },
        room,
    );
    let name_width = name.size().x;
    ui.painter()
        .galley(pos2(left, y - name.size().y / 2.0), name, p.text);
    let folder_room = room - name_width - space::XS;
    if !folder.is_empty() && folder_room > 24.0 {
        let folder = super::elided(
            ui,
            folder,
            font::regular(text::SM),
            p.text_faint,
            folder_room,
        );
        ui.painter().galley(
            pos2(left + name_width + space::XS, y - folder.size().y / 2.0),
            folder,
            p.text_faint,
        );
    }
    let response = response.on_hover_cursor(CursorIcon::PointingHand);
    response.clicked()
}

// the commented rows as a hunk of their own, with the range people read and the old and new lines it spans
#[allow(clippy::type_complexity)]
fn review_hunk(
    rows: &[(&DiffRow, &str)],
) -> Option<(
    String,
    Option<(usize, usize)>,
    Option<(usize, usize)>,
    String,
)> {
    let rows: Vec<&(&DiffRow, &str)> = rows
        .iter()
        .filter(|(r, _)| r.kind != RowKind::Hunk)
        .collect();
    let (first, last) = (rows.first()?.0, rows.last()?.0);
    let span = |side: fn(&DiffRow) -> Option<usize>| {
        let numbers: Vec<usize> = rows.iter().filter_map(|(r, _)| side(r)).collect();
        Some((*numbers.first()?, *numbers.last()?))
    };
    let (old, new) = (span(|r| r.old), span(|r| r.new));
    let count = |range: Option<(usize, usize)>| range.map_or(0, |(a, b)| b + 1 - a);
    let start = |range: Option<(usize, usize)>| range.map_or(0, |(a, _)| a);
    let mut hunk = format!(
        "@@ -{},{} +{},{} @@",
        start(old),
        count(old),
        start(new),
        count(new)
    );
    for (row, text) in &rows {
        hunk.push('\n');
        hunk.push(match row.kind {
            RowKind::Add => '+',
            RowKind::Remove => '-',
            _ => ' ',
        });
        hunk.push_str(text);
    }
    let marker = match first.kind {
        RowKind::Add if rows.iter().all(|(r, _)| r.kind == RowKind::Add) => "+",
        RowKind::Remove if rows.iter().all(|(r, _)| r.kind == RowKind::Remove) => "-",
        _ => "",
    };
    let (a, b) = (first.new.or(first.old)?, last.new.or(last.old)?);
    let range = if rows.len() == 1 || a == b {
        format!("{marker}{a}")
    } else {
        format!("{marker}{a} to {marker}{b}")
    };
    Some((range, old, new, hunk))
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
    let text = String::from_utf8(bytes).map_err(|_| "This file isn't text.".to_owned())?;
    Ok(mux_core::clip_lines(&text).into_owned())
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
    use super::{RowKind, link_target, parse_diff, review_hunk, split_lines};

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
    fn turns_commented_rows_into_a_hunk_of_their_own() {
        let unified = "@@ -10,3 +10,4 @@ pub fn auth()\n let a = 1;\n-let b = 2;\n+let b = 3;\n+let c = 4;\n let d = 5;\n";
        let (text, diff) = parse_diff(unified);
        let lines: Vec<&str> = text.split('\n').collect();
        let rows: Vec<_> = diff.rows.iter().zip(lines).collect();
        let (range, old, new, hunk) = review_hunk(&rows[3..5]).unwrap();
        assert_eq!(range, "+11 to +12");
        assert_eq!((old, new), (None, Some((11, 12))));
        assert_eq!(hunk, "@@ -0,0 +11,2 @@\n+let b = 3;\n+let c = 4;");
        // the hunk's own @@ row is left out, and mixed lines carry no sign in their range
        let (range, old, new, hunk) = review_hunk(&rows[0..3]).unwrap();
        assert_eq!(range, "10 to 11");
        assert_eq!((old, new), (Some((10, 11)), Some((10, 10))));
        assert_eq!(hunk, "@@ -10,2 +10,1 @@\n let a = 1;\n-let b = 2;");
        assert_eq!(review_hunk(&rows[2..3]).unwrap().0, "-11");
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
