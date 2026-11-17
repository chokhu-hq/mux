use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use mux_core::protocol::{FileKind, ServerMsg};
use mux_ui::{DirState, FilePreview, FileTree, PreviewAction, TreeAction};

use super::*;

// what one read asks the server for, the most a chunk can be
pub(crate) const CHUNK: u64 = 1024 * 1024;
const TREE: f32 = 280.0;
const SEARCH_LIMIT: u32 = 200;
const CHAT_ROOM: f32 = 420.0;

static NEXT_REQ: AtomicU64 = AtomicU64::new(1);

// each request to the server about files gets its own number, so its answer finds its way back
pub(crate) fn next_req() -> u64 {
    NEXT_REQ.fetch_add(1, Ordering::Relaxed)
}

enum Waiting {
    Dir(String),
    Read,
    Search,
}

// the files panel on the right: the session's folder as a tree, with the picked file previewed beside it
pub(crate) struct FilesPanel {
    pub session: u64,
    // the folder as the server names it
    root: String,
    tree: FileTree,
    preview: Option<FilePreview>,
    waiting: HashMap<u64, Waiting>,
}

impl FilesPanel {
    fn new(session: u64, root: String, expanded: Vec<String>) -> Self {
        let local = mux_core::paths::local(&root);
        Self {
            session,
            tree: FileTree::new(local, expanded.into_iter().collect()),
            root,
            preview: None,
            waiting: HashMap::new(),
        }
    }

    fn list(&mut self, outbox: &mut Vec<ClientMsg>, dir: String) {
        let req = next_req();
        self.tree.dirs.insert(dir.clone(), DirState::Loading);
        self.waiting.insert(req, Waiting::Dir(dir.clone()));
        outbox.push(ClientMsg::ListDir {
            req,
            root: self.root.clone(),
            dir,
        });
    }

    fn read(&mut self, outbox: &mut Vec<ClientMsg>, path: String, offset: u64) {
        let req = next_req();
        self.waiting.insert(req, Waiting::Read);
        outbox.push(ClientMsg::ReadFile {
            req,
            root: self.root.clone(),
            path,
            offset,
            len: CHUNK,
        });
    }

    fn open(&mut self, ctx: &egui::Context, outbox: &mut Vec<ClientMsg>, path: String) {
        let local = mux_core::paths::local(&mux_core::paths::join(&self.root, &path));
        let name = mux_core::paths::file_name(&self.root);
        self.tree.selected = Some(path.clone());
        self.preview = Some(FilePreview::new(ctx, path.clone(), local, name));
        self.read(outbox, path, 0);
    }

    // takes an answer meant for this panel, handing it back when it isn't
    fn reply(&mut self, msg: ServerMsg, outbox: &mut Vec<ClientMsg>) -> Option<ServerMsg> {
        let req = reply_req(&msg)?;
        let Some(waiting) = self.waiting.remove(&req) else {
            return Some(msg);
        };
        match (msg, waiting) {
            (
                ServerMsg::DirEntries {
                    dir,
                    entries,
                    truncated,
                    ..
                },
                _,
            ) => {
                self.tree
                    .dirs
                    .insert(dir, DirState::Loaded { entries, truncated });
            }
            (ServerMsg::PathMatches { query, paths, .. }, _) => {
                if query == self.tree.query.trim() {
                    self.tree.results = Some(paths);
                }
            }
            (
                ServerMsg::FileChunk {
                    path,
                    total,
                    offset,
                    bytes,
                    kind,
                    ..
                },
                _,
            ) => {
                if let Some(preview) = self.preview.as_mut().filter(|p| p.path == path) {
                    preview.add(offset, bytes, total, kind);
                    if preview.wants_more() {
                        let at = preview.loaded();
                        self.read(outbox, path, at);
                    }
                }
            }
            (ServerMsg::FileError { error, .. }, Waiting::Dir(dir)) => {
                self.tree.dirs.insert(dir, DirState::Failed(error));
            }
            (ServerMsg::FileError { error, .. }, Waiting::Read) => {
                if let Some(preview) = &mut self.preview {
                    preview.fail(error);
                }
            }
            (ServerMsg::FileError { .. }, Waiting::Search) => self.tree.results = Some(Vec::new()),
            _ => {}
        }
        None
    }
}

fn reply_req(msg: &ServerMsg) -> Option<u64> {
    match msg {
        ServerMsg::DirEntries { req, .. }
        | ServerMsg::FileChunk { req, .. }
        | ServerMsg::PathMatches { req, .. }
        | ServerMsg::FileError { req, .. } => Some(*req),
        _ => None,
    }
}

// `path` with `.` and `..` worked out, never climbing above where it starts
fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

// how a path is written after `@` in the composer, quoted when it has spaces
fn mention(path: &str) -> String {
    if path.contains(char::is_whitespace) {
        format!("@\"{path}\"")
    } else {
        format!("@{path}")
    }
}

impl App {
    pub(crate) fn toggle_files(&mut self) {
        if self.files_open {
            self.files_open = false;
            return;
        }
        let Some(id) = self.sessions.selected_id() else {
            return;
        };
        let Some((_, Some(root))) = self.sessions.checkout(id) else {
            return;
        };
        let same = self.files.as_ref().is_some_and(|f| f.root == root);
        if !same {
            let expanded = self.prefs.expanded.get(&root).cloned().unwrap_or_default();
            self.files = Some(FilesPanel::new(id, root, expanded));
        }
        if let Some(files) = &mut self.files {
            files.session = id;
        }
        self.files_open = true;
        self.viewer_open = false;
        self.pr_open = false;
    }

    // a file read through the server, for the panel or for a file opened from a chat link
    pub(crate) fn file_replies(&mut self) {
        for msg in std::mem::take(&mut self.sessions.file_replies) {
            let msg = match &mut self.files {
                Some(files) => files.reply(msg, &mut self.sessions.outbox),
                None => Some(msg),
            };
            let Some(msg) = msg else {
                continue;
            };
            let (Some(req), Some(view)) = (reply_req(&msg), &mut self.viewer) else {
                continue;
            };
            if self.viewer_read != Some(req) {
                continue;
            }
            self.viewer_read = None;
            match msg {
                ServerMsg::FileChunk {
                    bytes,
                    kind: FileKind::Text,
                    ..
                } => {
                    let text = String::from_utf8_lossy(&bytes);
                    // a chunk can end partway through a character
                    view.set_text(Ok(text.trim_end_matches('\u{fffd}')));
                }
                ServerMsg::FileChunk { .. } => {
                    view.set_text(Err("This file isn't text.".to_owned()));
                }
                ServerMsg::FileError { error, .. } => view.set_text(Err(error)),
                _ => {}
            }
        }
    }

    // the files panel, returning what's left of `main` for the session
    pub(crate) fn files_panel(&mut self, ui: &mut egui::Ui, main: Rect) -> Rect {
        let ctx = ui.ctx().clone();
        let open = self.files_open && self.files.is_some();
        let t = motion::spring_bool(&ctx, egui::Id::unique("files-panel"), open, Spring::SMOOTH)
            .clamp(0.0, 1.0);
        if t <= 0.001 {
            return main;
        }
        let full = self
            .files_width
            .clamp(420.0, (main.width() * 0.75).max(420.0))
            // the chat keeps room for its messages where the window has it
            .min((main.width() - CHAT_ROOM).max(360.0));
        let left = main.right() - full * t;
        let edge = if open { main.right() - full } else { left };
        let handle = Rect::from_min_max(
            pos2(left - 3.0, main.top()),
            pos2(left + 3.0, main.bottom()),
        );
        let drag = ui
            .interact(
                handle,
                egui::Id::unique("files-panel-resize"),
                egui::Sense::drag(),
            )
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if drag.dragged() {
            self.files_width = full - drag.drag_delta().x;
        }
        let panel = Rect::from_min_size(pos2(left, main.top()), vec2(full, main.height()));
        let mut pane = ui.new_child(UiBuilder::new().id_salt("files-panel").max_rect(panel));
        pane.set_clip_rect(panel.intersect(main).intersect(ui.clip_rect()));
        let Some(files) = &mut self.files else {
            return main;
        };
        for dir in files.tree.missing() {
            files.list(&mut self.sessions.outbox, dir);
        }

        let p = theme::palette(&pane);
        pane.painter().rect_filled(panel, 0.0, p.bg);
        pane.painter().vline(
            panel.left() + 0.5,
            panel.y_range(),
            egui::Stroke::new(1.0, p.border),
        );
        let header = Rect::from_min_size(panel.min, vec2(full, theme::control::HEADER));
        let mut close = false;
        let mut head = panel_header(&mut pane, header, "files-header", |ui| {
            close = IconButton::new(icons::X, "Close")
                .show(ui)
                .tip("Close")
                .clicked();
        });
        panel_title(&mut head, icons::FOLDER_OPEN, "Files");
        head.label(
            egui::RichText::new(mux_core::paths::file_name(&files.root))
                .font(font::regular(text::SM + 1.0))
                .color(p.text_faint),
        )
        .tip(mux_core::display_path(&files.root));

        let body = Rect::from_min_max(pos2(panel.left() + 1.0, header.bottom()), panel.max);
        let tree_width = TREE.min(body.width() * 0.45);
        let column = Rect::from_min_size(body.min, vec2(tree_width, body.height()));
        let mut tree_ui = pane.new_child(UiBuilder::new().id_salt("files-tree").max_rect(column));
        tree_ui.set_clip_rect(column.intersect(pane.clip_rect()));
        let tree_actions = files.tree.show(&mut tree_ui);
        pane.painter().vline(
            column.right() + 0.5,
            column.y_range(),
            egui::Stroke::new(1.0, p.border),
        );
        let view = Rect::from_min_max(pos2(column.right() + 1.0, body.top()), body.max);
        let mut view_ui = pane.new_child(UiBuilder::new().id_salt("files-view").max_rect(view));
        view_ui.set_clip_rect(view.intersect(pane.clip_rect()));
        let preview_action = match &mut files.preview {
            Some(preview) => preview.show(&mut view_ui),
            None => {
                view_ui.painter().text(
                    view.center(),
                    egui::Align2::CENTER_CENTER,
                    "Pick a file to preview it",
                    font::regular(text::MD),
                    p.text_faint,
                );
                None
            }
        };

        let mut save = false;
        for action in tree_actions {
            match action {
                TreeAction::List(dir) => files.list(&mut self.sessions.outbox, dir),
                TreeAction::Expanded => save = true,
                TreeAction::Open(path) => files.open(&ctx, &mut self.sessions.outbox, path),
                TreeAction::OpenInEditor(path) => {
                    let local = mux_core::paths::local(&mux_core::paths::join(&files.root, &path));
                    open_in_editor(&local, None);
                }
                TreeAction::Mention(path) => {
                    let mention = mention(&path);
                    ctx.copy_text(mention.clone());
                    let draft = &mut self.sessions.local(files.session).draft;
                    if !draft.is_empty() && !draft.ends_with(char::is_whitespace) {
                        draft.push(' ');
                    }
                    draft.push_str(&mention);
                    draft.push(' ');
                }
                TreeAction::Search(query) => {
                    let req = next_req();
                    files.waiting.insert(req, Waiting::Search);
                    self.sessions.outbox.push(ClientMsg::SearchPaths {
                        req,
                        root: files.root.clone(),
                        query,
                        limit: SEARCH_LIMIT,
                    });
                }
            }
        }
        let outbox = &mut self.sessions.outbox;
        if let (Some(action), Some(preview)) = (preview_action, &files.preview) {
            let local = preview.local.clone();
            match action {
                PreviewAction::LoadMore => {
                    let (path, at) = (preview.path.clone(), preview.loaded());
                    files.read(outbox, path, at);
                }
                PreviewAction::OpenExternally => mux_ui::chat::open_externally(&local),
                PreviewAction::OpenInEditor => open_in_editor(&local, None),
                PreviewAction::CopyPath => ctx.copy_text(local),
                PreviewAction::Reveal(dir) => {
                    files.tree.query.clear();
                    for dir in files.tree.reveal(&dir) {
                        files.list(outbox, dir);
                    }
                    save = true;
                }
                PreviewAction::Link(target) => {
                    if target.contains("://") {
                        ctx.open_url(egui::OpenUrl::new_tab(target));
                    } else {
                        let target = target.split('#').next().unwrap_or_default();
                        let folder = preview.path.rsplit_once('/').map_or("", |(f, _)| f);
                        let path = normalize(&format!("{folder}/{target}"));
                        files.open(&ctx, outbox, path);
                    }
                }
            }
        }
        if save {
            let mut expanded: Vec<String> = files.tree.expanded.iter().cloned().collect();
            expanded.sort();
            self.prefs.expanded.insert(files.root.clone(), expanded);
            self.prefs.save();
        }
        if close {
            self.files_open = false;
        }
        Rect::from_min_max(main.min, pos2(edge, main.bottom()))
    }
}

#[cfg(test)]
mod tests {
    use super::{mention, normalize};

    #[test]
    fn works_out_relative_links() {
        assert_eq!(normalize("docs/./guide/../intro.md"), "docs/intro.md");
        assert_eq!(normalize("/a.md"), "a.md");
        assert_eq!(normalize("../../a.md"), "a.md");
        assert_eq!(mention("src/main.rs"), "@src/main.rs");
        assert_eq!(mention("my notes.md"), "@\"my notes.md\"");
    }
}
