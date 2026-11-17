use std::collections::{HashMap, HashSet};

use egui::{Align2, Color32, CornerRadius, Rect, ScrollArea, Sense, Ui, WidgetInfo, pos2, vec2};
use mux_core::protocol::DirEntry;

use super::{MenuItem, copy_item, hover_t, search_field};
use crate::icons;
use crate::theme::{self, Palette, font, icon, space, text};

const ROW: f32 = 24.0;
const INDENT: f32 = 14.0;
const SEARCH: f32 = 36.0;

pub enum DirState {
    Loading,
    Loaded {
        entries: Vec<DirEntry>,
        truncated: bool,
    },
    Failed(String),
}

pub enum TreeAction {
    // a folder relative to the root that needs listing
    List(String),
    // the expanded folders changed, so they can be kept for the project
    Expanded,
    Open(String),
    OpenInEditor(String),
    // insert `@path` into the composer
    Mention(String),
    Search(String),
}

// a project's files as a tree, read a folder at a time as folders are opened
pub struct FileTree {
    // the root as this app can open it, for copying full paths
    pub local_root: String,
    pub dirs: HashMap<String, DirState>,
    pub expanded: HashSet<String>,
    pub selected: Option<String>,
    pub query: String,
    // matches for `searched`, which can lag behind what's typed
    pub results: Option<Vec<String>>,
    pub searched: String,
}

// one visible line of the tree
struct Row<'a> {
    depth: usize,
    path: String,
    entry: Option<&'a DirEntry>,
    // a note in place of a folder's entries, like loading or an error
    note: Option<(String, bool)>,
}

impl FileTree {
    pub fn new(local_root: String, expanded: HashSet<String>) -> Self {
        Self {
            local_root,
            dirs: HashMap::new(),
            expanded,
            selected: None,
            query: String::new(),
            results: None,
            searched: String::new(),
        }
    }

    // the folders that are open but not listed yet, the root first
    pub fn missing(&self) -> Vec<String> {
        std::iter::once(String::new())
            .chain(self.expanded.iter().cloned())
            .filter(|dir| !self.dirs.contains_key(dir))
            .collect()
    }

    // opens every folder down to `path`, so it shows in the tree
    pub fn reveal(&mut self, path: &str) -> Vec<String> {
        let mut at = String::new();
        let mut listing = Vec::new();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !at.is_empty() {
                at.push('/');
            }
            at.push_str(part);
            self.expanded.insert(at.clone());
            if !self.dirs.contains_key(&at) {
                self.dirs.insert(at.clone(), DirState::Loading);
                listing.push(at.clone());
            }
        }
        listing
    }

    fn rows(&self) -> Vec<Row<'_>> {
        let mut rows = Vec::new();
        self.walk("", 0, &mut rows);
        rows
    }

    fn walk<'a>(&'a self, dir: &str, depth: usize, rows: &mut Vec<Row<'a>>) {
        let note = |text: &str, error: bool| Row {
            depth,
            path: format!("{dir}/~note"),
            entry: None,
            note: Some((text.to_owned(), error)),
        };
        match self.dirs.get(dir) {
            None | Some(DirState::Loading) => rows.push(note("Loading…", false)),
            Some(DirState::Failed(error)) => rows.push(note(error, true)),
            Some(DirState::Loaded { entries, truncated }) => {
                if entries.is_empty() && depth == 0 {
                    rows.push(note("This folder is empty", false));
                }
                for entry in entries {
                    let path = if dir.is_empty() {
                        entry.name.clone()
                    } else {
                        format!("{dir}/{}", entry.name)
                    };
                    let open = entry.dir && self.expanded.contains(&path);
                    rows.push(Row {
                        depth,
                        path: path.clone(),
                        entry: Some(entry),
                        note: None,
                    });
                    if open {
                        self.walk(&path, depth + 1, rows);
                    }
                }
                if *truncated {
                    rows.push(note("Too many files to list them all", false));
                }
            }
        }
    }

    pub fn show(&mut self, ui: &mut Ui) -> Vec<TreeAction> {
        let p = theme::palette(ui);
        let mut actions = Vec::new();
        let rect = ui.max_rect();
        let search = Rect::from_min_size(rect.min, vec2(rect.width(), SEARCH))
            .shrink2(vec2(space::MD, space::XS));
        let before = self.query.clone();
        search_field(
            ui,
            search,
            egui::Id::unique("file-tree-search"),
            &mut self.query,
            "Search files",
        );
        if self.query != before {
            self.results = None;
            if !self.query.trim().is_empty() {
                actions.push(TreeAction::Search(self.query.trim().to_owned()));
            }
        }
        ui.painter().hline(
            rect.x_range(),
            rect.top() + SEARCH - 0.5,
            egui::Stroke::new(1.0, p.border),
        );
        let body = Rect::from_min_max(pos2(rect.left(), rect.top() + SEARCH), rect.max);
        let mut body_ui = ui.new_child(egui::UiBuilder::new().max_rect(body));
        body_ui.set_clip_rect(body.intersect(ui.clip_rect()));
        if self.query.trim().is_empty() {
            self.tree(&mut body_ui, p, &mut actions);
        } else {
            self.matches(&mut body_ui, p, &mut actions);
        }
        actions
    }

    fn tree(&mut self, ui: &mut Ui, p: &Palette, actions: &mut Vec<TreeAction>) {
        let mut toggle = None;
        let mut clicked = None;
        {
            let rows = self.rows();
            ScrollArea::vertical()
                .id_salt("file-tree")
                .auto_shrink(false)
                .show_rows(ui, ROW, rows.len(), |ui, range| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for row in &rows[range] {
                        let indent = space::SM + row.depth as f32 * INDENT;
                        let Some(entry) = row.entry else {
                            let (rect, _) = ui.allocate_exact_size(
                                vec2(ui.available_width(), ROW),
                                Sense::hover(),
                            );
                            let (note, error) = row.note.clone().unwrap_or_default();
                            ui.painter().text(
                                pos2(rect.left() + indent + icon::MD + space::SM, rect.center().y),
                                Align2::LEFT_CENTER,
                                note,
                                font::regular(text::SM + 1.0),
                                if error { p.danger } else { p.text_faint },
                            );
                            continue;
                        };
                        let open = entry.dir && self.expanded.contains(&row.path);
                        let selected = self.selected.as_deref() == Some(row.path.as_str());
                        let response = tree_row(ui, p, entry, indent, open, selected);
                        if response.clicked() {
                            if entry.dir {
                                toggle = Some(row.path.clone());
                            } else {
                                clicked = Some(row.path.clone());
                            }
                        }
                        self.menu(&response, &row.path, entry.dir, actions);
                    }
                });
        }
        if let Some(path) = toggle {
            if !self.expanded.remove(&path) {
                self.expanded.insert(path.clone());
                if !self.dirs.contains_key(&path) {
                    self.dirs.insert(path.clone(), DirState::Loading);
                    actions.push(TreeAction::List(path));
                }
            }
            actions.push(TreeAction::Expanded);
        }
        if let Some(path) = clicked {
            self.selected = Some(path.clone());
            actions.push(TreeAction::Open(path));
        }
    }

    fn matches(&mut self, ui: &mut Ui, p: &Palette, actions: &mut Vec<TreeAction>) {
        let Some(results) = &self.results else {
            ui.painter().text(
                ui.max_rect().center_top() + vec2(0.0, space::XL),
                Align2::CENTER_CENTER,
                "Searching…",
                font::regular(text::MD),
                p.text_faint,
            );
            return;
        };
        if results.is_empty() {
            ui.painter().text(
                ui.max_rect().center_top() + vec2(0.0, space::XL),
                Align2::CENTER_CENTER,
                "No files match",
                font::regular(text::MD),
                p.text_faint,
            );
            return;
        }
        let mut clicked = None;
        ScrollArea::vertical()
            .id_salt("file-matches")
            .auto_shrink(false)
            .show_rows(ui, ROW + 4.0, results.len(), |ui, range| {
                ui.spacing_mut().item_spacing.y = 0.0;
                for path in &results[range] {
                    let selected = self.selected.as_deref() == Some(path.as_str());
                    let response = match_row(ui, p, path, selected);
                    if response.clicked() {
                        clicked = Some(path.clone());
                    }
                    self.menu(&response, path, false, actions);
                }
            });
        if let Some(path) = clicked {
            self.selected = Some(path.clone());
            actions.push(TreeAction::Open(path));
        }
    }

    fn menu(
        &self,
        response: &egui::Response,
        path: &str,
        dir: bool,
        actions: &mut Vec<TreeAction>,
    ) {
        response.context_menu(|ui| {
            if !dir && MenuItem::new("Open").icon(icons::FILE).show(ui).clicked() {
                actions.push(TreeAction::Open(path.to_owned()));
                ui.close();
            }
            if MenuItem::new("Open in editor")
                .icon(icons::ARROW_SQUARE_OUT)
                .show(ui)
                .clicked()
            {
                actions.push(TreeAction::OpenInEditor(path.to_owned()));
                ui.close();
            }
            copy_item(
                ui,
                "Copy path",
                &mux_core::paths::join(&self.local_root, path),
            );
            copy_item(ui, "Copy relative path", path);
            if MenuItem::new("Copy as @ mention")
                .icon(icons::PAPER_PLANE_RIGHT)
                .show(ui)
                .clicked()
            {
                actions.push(TreeAction::Mention(path.to_owned()));
                ui.close();
            }
        });
    }
}

fn git_color(p: &Palette, mark: char) -> Color32 {
    match mark {
        'A' | 'U' => p.success,
        'D' | '!' => p.danger,
        'R' | 'C' => p.accent,
        _ => p.warning,
    }
}

fn row_back(ui: &Ui, p: &Palette, rect: Rect, response: &egui::Response, selected: bool) {
    let hover = hover_t(ui, response);
    let fill = if selected {
        theme::tint(p, theme::fill::SELECTED)
    } else {
        theme::tint(p, theme::fill::HOVER * hover)
    };
    ui.painter().rect_filled(
        rect.shrink2(vec2(space::XS, 1.0)),
        CornerRadius::same(theme::radius::MD),
        fill,
    );
}

fn tree_row(
    ui: &mut Ui,
    p: &Palette,
    entry: &DirEntry,
    indent: f32,
    open: bool,
    selected: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let name = entry.name.clone();
    response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, name.clone()));
    row_back(ui, p, rect, &response, selected);
    let faded = |c: Color32| {
        if entry.ignored {
            c.gamma_multiply(0.55)
        } else {
            c
        }
    };
    let mut x = rect.left() + indent;
    if entry.dir {
        ui.painter().text(
            pos2(x + icon::SM / 2.0, rect.center().y),
            Align2::CENTER_CENTER,
            if open {
                icons::CARET_DOWN
            } else {
                icons::CARET_RIGHT
            },
            font::icon(icon::SM),
            p.text_faint,
        );
    }
    x += icon::SM + space::XS;
    ui.painter().text(
        pos2(x + icon::MD / 2.0, rect.center().y),
        Align2::CENTER_CENTER,
        match (entry.dir, open) {
            (true, true) => icons::FOLDER_OPEN,
            (true, false) => icons::FOLDER,
            _ => icons::FILE,
        },
        font::icon(icon::MD),
        faded(p.text_muted),
    );
    x += icon::MD + space::SM;
    let mut right = rect.right() - space::MD;
    if let Some(mark) = entry.git {
        let galley = ui.painter().text(
            pos2(right, rect.center().y),
            Align2::RIGHT_CENTER,
            mark,
            font::medium(text::SM + 1.0),
            git_color(p, mark),
        );
        right = galley.left() - space::SM;
    }
    let color = match entry.git {
        Some(mark) if !entry.dir => git_color(p, mark),
        _ => p.text,
    };
    let galley = super::elided(
        ui,
        entry.name.clone(),
        font::regular(text::MD),
        faded(color),
        (right - x).max(0.0),
    );
    ui.painter().galley(
        pos2(x, rect.center().y - galley.size().y / 2.0),
        galley,
        faded(color),
    );
    response
}

fn match_row(ui: &mut Ui, p: &Palette, path: &str, selected: bool) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), ROW + 4.0), Sense::click());
    let label = path.to_owned();
    response.widget_info(|| WidgetInfo::labeled(egui::Role::Button, true, label.clone()));
    row_back(ui, p, rect, &response, selected);
    let (name, folder) = match path.rsplit_once('/') {
        Some((folder, name)) => (name, folder),
        None => (path, ""),
    };
    let x = rect.left() + space::MD;
    ui.painter().text(
        pos2(x + icon::MD / 2.0, rect.center().y),
        Align2::CENTER_CENTER,
        icons::FILE,
        font::icon(icon::MD),
        p.text_muted,
    );
    let x = x + icon::MD + space::SM;
    let name = ui.painter().text(
        pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        name,
        font::regular(text::MD),
        p.text,
    );
    let folder = super::elided(
        ui,
        folder.to_owned(),
        font::regular(text::SM + 1.0),
        p.text_faint,
        (rect.right() - space::MD - name.right() - space::SM).max(0.0),
    );
    ui.painter().galley(
        pos2(
            name.right() + space::SM,
            rect.center().y - folder.size().y / 2.0,
        ),
        folder,
        p.text_faint,
    );
    response
}
