use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};
use egui::text::LayoutJob;
use egui::{
    Align, Align2, Context, CornerRadius, FontData, FontFamily, FontId, Frame, Image, Layout,
    Margin, Rect, RichText, ScrollArea, Sense, Stroke, Ui, UiBuilder, pos2, vec2,
};
use egui_extras::{Column, TableBuilder};
use mux_core::protocol::FileKind;

use super::file_formats::{
    Cell, Notebook, Output, Shape, describe, hex_line, parse_notebook, parse_table, shape,
    size_label,
};
use super::{Button, FileView, IconButton, ImageViewer, Segmented, Size, TooltipExt};
use crate::icons;
use crate::theme::{self, Palette, font, radius, space, text};

// pictures, notebooks and tables are read whole up to this size, past which they're shown in part as text
const WHOLE: u64 = 32 * 1024 * 1024;
const HEX: usize = 64 * 1024;
const MAX_TABLE_ROWS: usize = 200_000;
const MAX_COLUMNS: usize = 200;
const CRUMBS: f32 = 40.0;

static NEXT: AtomicU64 = AtomicU64::new(0);

pub enum PreviewAction {
    LoadMore,
    OpenExternally,
    OpenInEditor,
    CopyPath,
    // a folder in the breadcrumbs, relative to the root, to show in the tree
    Reveal(String),
    // a link clicked in rendered markdown
    Link(String),
}

// what's slow to build from the bytes, made on another thread and tagged with the theme it was coloured for
enum Prepared {
    Table(Vec<Vec<String>>),
    Notebook(Result<(Notebook, Vec<Option<LayoutJob>>), String>),
}

// one file from the tree, read through the server in chunks and shown according to what it holds
pub struct FilePreview {
    // relative to the root
    pub path: String,
    // as this app can open it, for an editor or another app
    pub local: String,
    root_name: String,
    kind: Option<FileKind>,
    total: u64,
    bytes: Vec<u8>,
    error: Option<String>,
    // markdown shows its source rather than rendered
    source: bool,
    text: Option<FileView>,
    // how many bytes the text view was built from
    text_len: usize,
    prepared: Arc<Mutex<Option<(Palette, Prepared)>>>,
    preparing: Option<Palette>,
    // image and notebook pictures handed to egui, forgotten again when the preview goes
    uris: Vec<String>,
    font: Option<Result<String, String>>,
    id: u64,
    ctx: Context,
}

impl Drop for FilePreview {
    fn drop(&mut self) {
        for uri in &self.uris {
            self.ctx.forget_image(uri);
        }
    }
}

impl FilePreview {
    pub fn new(ctx: &Context, path: String, local: String, root_name: String) -> Self {
        Self {
            path,
            local,
            root_name,
            kind: None,
            total: 0,
            bytes: Vec::new(),
            error: None,
            source: false,
            text: None,
            text_len: 0,
            prepared: Arc::default(),
            preparing: None,
            uris: Vec::new(),
            font: None,
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            ctx: ctx.clone(),
        }
    }

    pub fn loaded(&self) -> u64 {
        self.bytes.len() as u64
    }

    pub fn add(&mut self, offset: u64, bytes: Vec<u8>, total: u64, kind: FileKind) {
        if offset != self.loaded() {
            return;
        }
        self.bytes.extend_from_slice(&bytes);
        self.total = total;
        self.kind = Some(kind);
        // a file that shrank while being read ends here
        if bytes.is_empty() {
            self.total = self.loaded();
        }
    }

    pub fn fail(&mut self, error: String) {
        self.error = Some(error);
    }

    fn shape(&self) -> Option<Shape> {
        let shape = shape(
            &self.path,
            self.kind?,
            &self.bytes[..self.bytes.len().min(64)],
        );
        // too big to read whole, so the start of it is shown as text or bytes
        Some(match shape {
            Shape::Markdown | Shape::Table(_) | Shape::Notebook if self.total > WHOLE => {
                Shape::Code
            }
            Shape::Image | Shape::Svg | Shape::Font if self.total > WHOLE => Shape::Hex,
            shape => shape,
        })
    }

    // still reading the rest of a file that's only shown once it's whole
    pub fn wants_more(&self) -> bool {
        let whole = matches!(
            self.shape(),
            Some(
                Shape::Markdown
                    | Shape::Table(_)
                    | Shape::Notebook
                    | Shape::Image
                    | Shape::Svg
                    | Shape::Font
            )
        );
        self.error.is_none() && whole && self.loaded() < self.total
    }

    // the text read so far, leaving off a character cut in half at the end of the last chunk
    fn text(&self) -> &str {
        match std::str::from_utf8(&self.bytes) {
            Ok(text) => text,
            Err(e) => std::str::from_utf8(&self.bytes[..e.valid_up_to()]).unwrap_or_default(),
        }
    }

    pub fn show(&mut self, ui: &mut Ui) -> Option<PreviewAction> {
        let p = theme::palette(ui);
        let rect = ui.max_rect();
        let header = Rect::from_min_size(rect.min, vec2(rect.width(), CRUMBS));
        let mut action = self.header(ui, header.shrink2(vec2(space::MD, 0.0)));
        ui.painter().hline(
            header.x_range(),
            header.bottom() - 0.5,
            Stroke::new(1.0, p.border),
        );
        let body = Rect::from_min_max(pos2(rect.left(), header.bottom()), rect.max);
        let mut body_ui = ui.new_child(
            UiBuilder::new()
                .id_salt(("preview", self.id))
                .max_rect(body),
        );
        body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
        action = self.body(&mut body_ui).or(action);
        action
    }

    fn header(&mut self, ui: &mut Ui, rect: Rect) -> Option<PreviewAction> {
        let p = theme::palette(ui);
        let mut action = None;
        let mut row = ui.new_child(
            UiBuilder::new()
                .id_salt("preview-actions")
                .max_rect(rect)
                .layout(Layout::right_to_left(Align::Center)),
        );
        row.spacing_mut().item_spacing.x = space::XS;
        let shape = self.shape();
        let code = matches!(
            shape,
            Some(Shape::Code | Shape::Markdown | Shape::Table(_) | Shape::Mermaid)
        );
        if code {
            if IconButton::new(icons::ARROW_SQUARE_OUT, "Open in editor")
                .show(&mut row)
                .tip("Open in editor")
                .clicked()
            {
                action = Some(PreviewAction::OpenInEditor);
            }
        } else if IconButton::new(icons::ARROW_SQUARE_OUT, "Open externally")
            .show(&mut row)
            .tip("Open in its own app")
            .clicked()
        {
            action = Some(PreviewAction::OpenExternally);
        }
        if IconButton::new(icons::COPY, "Copy path")
            .show(&mut row)
            .tip("Copy path")
            .clicked()
        {
            action = Some(PreviewAction::CopyPath);
        }
        if shape == Some(Shape::Markdown) {
            row.add_space(space::SM);
            let mut source = self.source;
            Segmented::new(&mut source, &[(false, "Rendered"), (true, "Source")]).show(&mut row);
            self.source = source;
        }
        if self.kind.is_some() {
            row.add_space(space::SM);
            row.label(
                RichText::new(size_label(self.total))
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_faint),
            );
        }
        let right = row.min_rect().left() - space::SM;

        let mut crumbs = ui.new_child(
            UiBuilder::new()
                .id_salt("preview-crumbs")
                .max_rect(Rect::from_min_max(rect.min, pos2(right, rect.bottom())))
                .layout(Layout::left_to_right(Align::Center)),
        );
        crumbs.set_clip_rect(crumbs.max_rect().intersect(ui.clip_rect()));
        crumbs.spacing_mut().item_spacing.x = space::XS;
        let parts: Vec<&str> = self.path.split('/').filter(|s| !s.is_empty()).collect();
        let crumb = |ui: &mut Ui, label: &str, last: bool| {
            let color = if last { p.text } else { p.text_faint };
            let font = if last {
                font::medium(text::MD)
            } else {
                font::regular(text::MD)
            };
            let sense = if last { Sense::hover() } else { Sense::click() };
            let response =
                ui.add(egui::Label::new(RichText::new(label).font(font).color(color)).sense(sense));
            if last {
                return false;
            }
            response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
        };
        let separator = |ui: &mut Ui| {
            ui.label(
                RichText::new("›")
                    .font(font::regular(text::MD))
                    .color(p.text_faint),
            );
        };
        if crumb(&mut crumbs, &self.root_name, parts.is_empty()) {
            action = Some(PreviewAction::Reveal(String::new()));
        }
        for (i, part) in parts.iter().enumerate() {
            separator(&mut crumbs);
            if crumb(&mut crumbs, part, i + 1 == parts.len()) {
                action = Some(PreviewAction::Reveal(parts[..=i].join("/")));
            }
        }
        action
    }

    fn body(&mut self, ui: &mut Ui) -> Option<PreviewAction> {
        let p = theme::palette(ui);
        if let Some(error) = &self.error {
            notice(ui, error, None);
            return None;
        }
        let Some(shape) = self.shape() else {
            notice(ui, "Loading…", None);
            return None;
        };
        if self.wants_more() {
            let done = self.loaded() as f64 / self.total.max(1) as f64;
            notice(ui, &format!("Loading… {:.0}%", done * 100.0), None);
            return None;
        }
        match shape {
            Shape::Code => return self.code(ui),
            Shape::Markdown if self.source => return self.code(ui),
            Shape::Markdown => {
                let mut link = None;
                ScrollArea::vertical()
                    .id_salt(("markdown", self.id))
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        Frame::new()
                            .inner_margin(Margin::symmetric(20, 16))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                link = super::chat::markdown(ui, self.text());
                            });
                    });
                return link.map(PreviewAction::Link);
            }
            Shape::Table(delimiter) => self.table(ui, delimiter, p),
            Shape::Notebook => self.notebook(ui, p),
            Shape::Mermaid => {
                let rect = ui.max_rect();
                let top = Rect::from_min_size(rect.min, vec2(rect.width(), 120.0));
                let mut top_ui = ui.new_child(UiBuilder::new().max_rect(top));
                let open = notice(
                    &mut top_ui,
                    "Mermaid diagrams can't be drawn here. Preview not available, open externally.",
                    Some("Open"),
                );
                let rest = Rect::from_min_max(pos2(rect.left(), top.bottom()), rect.max);
                ui.painter()
                    .hline(rest.x_range(), rest.top(), Stroke::new(1.0, p.border));
                let mut rest_ui = ui.new_child(UiBuilder::new().max_rect(rest));
                let code = self.code(&mut rest_ui);
                if open {
                    return Some(PreviewAction::OpenExternally);
                }
                return code;
            }
            Shape::Image | Shape::Svg => self.image(ui, shape),
            Shape::Font => self.font(ui),
            Shape::Pdf => {
                if notice(
                    ui,
                    "PDFs can't be shown here. Preview not available, open externally.",
                    Some("Open"),
                ) {
                    return Some(PreviewAction::OpenExternally);
                }
            }
            Shape::Office => {
                if notice(
                    ui,
                    "Office documents can't be shown here. Preview not available, open externally.",
                    Some("Open"),
                ) {
                    return Some(PreviewAction::OpenExternally);
                }
            }
            Shape::Hex => self.hex(ui),
        }
        None
    }

    // highlighted text, with a strip to read more of a file that's bigger than what's loaded
    fn code(&mut self, ui: &mut Ui) -> Option<PreviewAction> {
        let p = theme::palette(ui);
        if self.text.is_none() || self.text_len != self.bytes.len() {
            let mut view = FileView::text(self.path.clone(), self.text());
            if let Some(old) = &self.text {
                view.wrap = old.wrap;
            }
            self.text = Some(view);
            self.text_len = self.bytes.len();
        }
        let rect = ui.max_rect();
        let more = self.loaded() < self.total;
        let strip = if more { 40.0 } else { 0.0 };
        let area = Rect::from_min_max(rect.min, pos2(rect.right(), rect.bottom() - strip));
        let mut code_ui = ui.new_child(UiBuilder::new().id_salt("code").max_rect(area));
        code_ui.set_clip_rect(area.intersect(ui.clip_rect()));
        if let Some(view) = &mut self.text {
            view.body(&mut code_ui);
        }
        if !more {
            return None;
        }
        let bar = Rect::from_min_max(pos2(rect.left(), area.bottom()), rect.max);
        ui.painter()
            .hline(bar.x_range(), bar.top(), Stroke::new(1.0, p.border));
        let mut bar_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(bar.shrink2(vec2(space::MD, 0.0)))
                .layout(Layout::right_to_left(Align::Center)),
        );
        let clicked = Button::new("Load more")
            .size(Size::Sm)
            .show(&mut bar_ui)
            .clicked();
        bar_ui.label(
            RichText::new(format!(
                "Showing {} of {}",
                size_label(self.loaded()),
                size_label(self.total)
            ))
            .font(font::regular(text::SM + 1.0))
            .color(p.text_faint),
        );
        clicked.then_some(PreviewAction::LoadMore)
    }

    // parses on another thread, colouring code for the theme it's shown in
    fn prepared(&mut self, ui: &Ui, shape: Shape) -> bool {
        let palette = *theme::palette(ui);
        let ready = self
            .prepared
            .lock()
            .is_ok_and(|slot| slot.as_ref().is_some_and(|(made, _)| *made == palette));
        if ready {
            return true;
        }
        if self.preparing != Some(palette) {
            self.preparing = Some(palette);
            let (slot, ctx, text) = (
                self.prepared.clone(),
                ui.ctx().clone(),
                self.text().to_owned(),
            );
            let dark = ui.visuals().dark_mode;
            std::thread::spawn(move || {
                let made = match shape {
                    Shape::Table(delimiter) => {
                        Prepared::Table(parse_table(&text, delimiter, MAX_TABLE_ROWS))
                    }
                    _ => Prepared::Notebook(parse_notebook(&text).map(|notebook| {
                        let jobs = notebook
                            .cells
                            .iter()
                            .map(|cell| match cell {
                                Cell::Code { source, .. } => crate::highlight::colorize(
                                    source,
                                    &notebook.language,
                                    &palette,
                                    dark,
                                    font::mono(text::MONO_SM),
                                ),
                                _ => None,
                            })
                            .collect();
                        (notebook, jobs)
                    })),
                };
                if let Ok(mut slot) = slot.lock() {
                    *slot = Some((palette, made));
                }
                ctx.request_repaint();
            });
        }
        false
    }

    fn table(&mut self, ui: &mut Ui, delimiter: u8, p: &Palette) {
        if !self.prepared(ui, Shape::Table(delimiter)) {
            notice(ui, "Reading the table…", None);
            return;
        }
        let slot = self.prepared.lock();
        let Some((_, Prepared::Table(rows))) = slot.as_ref().ok().and_then(|s| s.as_ref()) else {
            return;
        };
        let Some((head, rows)) = rows.split_first() else {
            notice(ui, "This table is empty.", None);
            return;
        };
        let columns = rows
            .iter()
            .take(1000)
            .map(Vec::len)
            .chain([head.len()])
            .max()
            .unwrap_or(0)
            .min(MAX_COLUMNS);
        let digits = rows.len().to_string().len() as f32;
        let cell = |ui: &mut Ui, value: &str, strong: bool| {
            let rich = RichText::new(value.replace('\n', " ")).font(if strong {
                font::medium(text::MD)
            } else {
                font::regular(text::MD)
            });
            ui.add(
                egui::Label::new(rich.color(if strong { p.text } else { p.text_muted })).truncate(),
            );
        };
        let rect = ui.max_rect().shrink2(vec2(space::SM, space::XS));
        let mut inner = ui.new_child(UiBuilder::new().max_rect(rect));
        ScrollArea::horizontal()
            .id_salt(("table", self.id))
            .auto_shrink(false)
            .show(&mut inner, |ui| {
                let mut table = TableBuilder::new(ui)
                    .id_salt(("table-body", self.id))
                    .striped(true)
                    .resizable(true)
                    .auto_shrink(false)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(Column::exact(digits * 8.0 + space::MD));
                for _ in 0..columns {
                    table = table.column(Column::initial(140.0).at_least(40.0).clip(true));
                }
                table
                    .header(26.0, |mut header| {
                        header.col(|_| {});
                        for i in 0..columns {
                            header.col(|ui| cell(ui, head.get(i).map_or("", String::as_str), true));
                        }
                    })
                    .body(|body| {
                        body.rows(24.0, rows.len(), |mut row| {
                            let index = row.index();
                            let fields = &rows[index];
                            row.col(|ui| {
                                ui.label(
                                    RichText::new((index + 1).to_string())
                                        .font(font::mono(text::MONO_SM))
                                        .color(p.text_faint),
                                );
                            });
                            for i in 0..columns {
                                row.col(|ui| {
                                    cell(ui, fields.get(i).map_or("", String::as_str), false)
                                });
                            }
                        });
                    });
            });
    }

    fn notebook(&mut self, ui: &mut Ui, p: &Palette) {
        if !self.prepared(ui, Shape::Notebook) {
            notice(ui, "Reading the notebook…", None);
            return;
        }
        let slot = self.prepared.lock();
        let Some((_, Prepared::Notebook(notebook))) = slot.as_ref().ok().and_then(|s| s.as_ref())
        else {
            return;
        };
        let (notebook, jobs) = match notebook {
            Ok(built) => built,
            Err(error) => {
                notice(ui, error, None);
                return;
            }
        };
        let mut uris = Vec::new();
        ScrollArea::vertical()
            .id_salt(("notebook", self.id))
            .auto_shrink(false)
            .show(ui, |ui| {
                Frame::new()
                    .inner_margin(Margin::symmetric(16, 12))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing.y = space::SM;
                        for (i, cell) in notebook.cells.iter().enumerate() {
                            match cell {
                                Cell::Markdown(source) => {
                                    super::chat::markdown(ui, source);
                                }
                                Cell::Raw(source) => {
                                    ui.label(
                                        RichText::new(source)
                                            .font(font::mono(text::MONO_SM))
                                            .color(p.text_muted),
                                    );
                                }
                                Cell::Code { source, outputs } => {
                                    let job = jobs.get(i).cloned().flatten();
                                    code_cell(ui, source, job, p);
                                    for (n, output) in outputs.iter().enumerate() {
                                        match output {
                                            Output::Text(text) => {
                                                ui.label(
                                                    RichText::new(text.trim_end())
                                                        .font(font::mono(text::MONO_SM))
                                                        .color(p.text_muted),
                                                );
                                            }
                                            Output::Error(text) => {
                                                ui.label(
                                                    RichText::new(text.trim_end())
                                                        .font(font::mono(text::MONO_SM))
                                                        .color(p.danger),
                                                );
                                            }
                                            Output::Png(bytes) => {
                                                let uri = format!(
                                                    "bytes://mux-notebook/{}/{i}-{n}.png",
                                                    self.id
                                                );
                                                if !self.uris.contains(&uri) {
                                                    ui.ctx()
                                                        .include_bytes(uri.clone(), bytes.clone());
                                                    uris.push(uri.clone());
                                                }
                                                ui.add(
                                                    Image::from_uri(uri)
                                                        .max_width(ui.available_width())
                                                        .fit_to_original_size(1.0),
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            ui.add_space(space::SM);
                        }
                    });
            });
        drop(slot);
        self.uris.extend(uris);
    }

    // the picture fitted to the pane, opening in the full viewer on a click
    fn image(&mut self, ui: &mut Ui, shape: Shape) {
        let ext = if shape == Shape::Svg { "svg" } else { "img" };
        let uri = format!("bytes://mux-preview/{}/file.{ext}", self.id);
        if !self.uris.contains(&uri) {
            ui.ctx().include_bytes(uri.clone(), self.bytes.clone());
            self.uris.push(uri.clone());
        }
        let rect = ui.max_rect().shrink(space::LG);
        let image = Image::from_uri(uri)
            .max_size(rect.size())
            .fit_to_original_size(1.0)
            .sense(Sense::click());
        let mut inner = ui.new_child(
            UiBuilder::new()
                .max_rect(rect)
                .layout(Layout::centered_and_justified(egui::Direction::TopDown)),
        );
        let response = inner.add(image);
        if response.on_hover_cursor(egui::CursorIcon::ZoomIn).clicked() {
            ImageViewer::open(ui.ctx(), vec![self.local.clone()], 0);
        }
    }

    // the font's own letters at a few sizes, once egui has taken it in under a name of its own
    fn font(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui);
        let font = self.font.get_or_insert_with(|| {
            skrifa::FontRef::from_index(&self.bytes, 0)
                .map_err(|_| "This font couldn't be read.".to_owned())?;
            let name = format!("mux-preview-font-{}", self.id);
            ui.ctx().add_font(FontInsert::new(
                &name,
                FontData::from_owned(self.bytes.clone()),
                vec![InsertFontFamily {
                    family: FontFamily::Name(name.clone().into()),
                    priority: FontPriority::Highest,
                }],
            ));
            Ok(name)
        });
        let name = match font {
            Ok(name) => name.clone(),
            Err(error) => {
                notice(ui, &error.clone(), None);
                return;
            }
        };
        let family = FontFamily::Name(name.into());
        // egui takes a new font in at the start of the next frame
        let ready = ui
            .ctx()
            .fonts(|f| f.definitions().families.contains_key(&family));
        if !ready {
            ui.ctx().request_repaint();
            return;
        }
        ScrollArea::vertical()
            .id_salt(("font", self.id))
            .auto_shrink(false)
            .show(ui, |ui| {
                Frame::new()
                    .inner_margin(Margin::symmetric(20, 16))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        for size in [14.0, 20.0, 28.0, 40.0, 56.0] {
                            ui.label(
                                RichText::new(format!("{size:.0} px"))
                                    .font(font::regular(text::SM))
                                    .color(p.text_faint),
                            );
                            ui.label(
                                RichText::new("The quick brown fox jumps over the lazy dog")
                                    .font(FontId::new(size, family.clone()))
                                    .color(p.text),
                            );
                            ui.add_space(space::SM);
                        }
                        ui.label(
                            RichText::new("ABCDEFGHIJKLMNOPQRSTUVWXYZ\nabcdefghijklmnopqrstuvwxyz\n0123456789 !?&@#%*()[]{}")
                                .font(FontId::new(24.0, family.clone()))
                                .color(p.text_muted),
                        );
                    });
            });
    }

    // the first 64 KB as offsets, bytes and the characters they read as
    fn hex(&mut self, ui: &mut Ui) {
        let p = theme::palette(ui);
        let shown = &self.bytes[..self.bytes.len().min(HEX)];
        let rect = ui.max_rect();
        let head = Rect::from_min_size(rect.min, vec2(rect.width(), 32.0));
        let what = describe(&self.bytes, &self.path);
        let mut summary = format!("{what} · {}", size_label(self.total));
        if self.total as usize > shown.len() {
            summary.push_str(&format!(
                " · first {} shown",
                size_label(shown.len() as u64)
            ));
        }
        ui.painter().text(
            pos2(head.left() + space::MD, head.center().y),
            Align2::LEFT_CENTER,
            summary,
            font::regular(text::SM + 1.0),
            p.text_faint,
        );
        let body = Rect::from_min_max(pos2(rect.left(), head.bottom()), rect.max);
        let mut body_ui =
            ui.new_child(UiBuilder::new().max_rect(body.shrink2(vec2(space::MD, 0.0))));
        let rows = shown.len().div_ceil(16);
        let row_height = 18.0;
        ScrollArea::both()
            .id_salt(("hex", self.id))
            .auto_shrink(false)
            .show_rows(&mut body_ui, row_height, rows, |ui, range| {
                for row in range {
                    let start = row * 16;
                    let line = hex_line(&shown[start..(start + 16).min(shown.len())], start);
                    ui.add_sized(
                        vec2(ui.available_width().max(560.0), row_height),
                        egui::Label::new(
                            RichText::new(line)
                                .font(font::mono(text::MONO_SM))
                                .color(p.text_muted),
                        )
                        .wrap_mode(egui::TextWrapMode::Extend)
                        .halign(Align::Min),
                    );
                }
            });
    }
}

fn code_cell(ui: &mut Ui, source: &str, job: Option<LayoutJob>, p: &Palette) {
    Frame::new()
        .fill(theme::tint(p, theme::fill::CARD))
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(radius::MD))
        .inner_margin(Margin::symmetric(10, 8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut job = job.unwrap_or_else(|| {
                LayoutJob::simple(
                    source.to_owned(),
                    font::mono(text::MONO_SM),
                    p.text,
                    f32::INFINITY,
                )
            });
            job.wrap.max_width = ui.available_width();
            ui.label(job);
        });
}

// a quiet message in the middle of the pane, with a button under it when one's given
fn notice(ui: &mut Ui, message: &str, button: Option<&str>) -> bool {
    let p = theme::palette(ui);
    let rect = ui.max_rect();
    let mut inner = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink(space::XL))
            .layout(Layout::top_down(Align::Center)),
    );
    inner.add_space((rect.height() / 2.0 - 60.0).clamp(0.0, 200.0));
    inner.label(
        RichText::new(message)
            .font(font::regular(text::MD))
            .color(p.text_muted),
    );
    let mut clicked = false;
    if let Some(label) = button {
        inner.add_space(space::MD);
        clicked = Button::new(label).size(Size::Sm).show(&mut inner).clicked();
    }
    clicked
}
