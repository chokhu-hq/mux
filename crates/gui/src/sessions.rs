use std::collections::HashMap;

use eframe::egui::{
    self, CornerRadius, CursorIcon, Frame, Id, Key, Modifiers, Order, Rect, Sense, Stroke,
    UiBuilder, Vec2, pos2, vec2,
};
use mux_core::protocol::{
    self as wire, AgentItem, AgentSettings, ClientMsg, GroupInfo, ItemStatus, ModelOption,
    ServerMsg, SessionInfo, UsageWindow,
};
use mux_core::{Provider, SessionKind, SessionStatus};
use mux_ui::chat::{
    self, ChatItem, Decision, DiffKind, DiffLine, FileDiff, PermissionRequest, ToolCall, ToolKind,
    ToolStatus,
};
use mux_ui::motion::{Spring, spring_bool};
use mux_ui::theme::{self, radius};
use mux_ui::{GroupHeader, MenuItem, SessionRow, TextInput, TooltipExt, icons};

use crate::terminal::Terminal;

// the models each agent cli offers
pub type Models = HashMap<Provider, Vec<ModelOption>>;

const ROW_GAP: f32 = 2.0;
const GROUP_GAP: f32 = 6.0;

// per session state that only this window cares about
#[derive(Default)]
pub struct Local {
    pub draft: String,
    pub terminal: Option<Terminal>,
    pub attaching: bool,
    // agent sessions: the server's transcript, and the chat items built from it
    pub opened: bool,
    pub busy: bool,
    pub settings: AgentSettings,
    transcript: Vec<AgentItem>,
    pub chat: Vec<ChatItem>,
    // which approval each chat item answers, so a click can be sent back to the right one
    pub approvals: Vec<Option<u64>>,
}

impl Local {
    // an agent can only switch cli before its first message, since neither can pick up the other's history
    pub fn has_started(&self) -> bool {
        !self.transcript.is_empty()
    }

    // rebuilt only when the transcript changes, not every frame
    fn rebuild(&mut self) {
        (self.chat, self.approvals) = chat_items(&self.transcript, self.busy);
    }
}

struct Drag {
    id: u64,
    grab: Vec2,
    width: f32,
}

#[derive(Clone, Copy)]
struct Drop {
    group: usize,
    index: usize,
}

enum Action {
    Toggle(u64),
    StartRename(u64, String),
    Rename(u64, String),
    EndRename,
    ChangeFolder(u64),
    RemoveGroup(u64),
    Select(u64),
    Remove(u64),
    StartDrag(Drag),
    NewAgent(u64),
}

#[derive(Default)]
pub struct Sessions {
    groups: Vec<GroupInfo>,
    locals: HashMap<u64, Local>,
    selected: Option<u64>,
    drag: Option<Drag>,
    loaded: bool,
    // the group whose name is being edited inline, with the draft name
    renaming: Option<(u64, String)>,
    // the models each agent cli offers, sent by the server once it has asked
    models: Models,
    usage: HashMap<Provider, Vec<UsageWindow>>,
    pub outbox: Vec<ClientMsg>,
}

impl Sessions {
    pub fn apply(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome { .. } => {}
            ServerMsg::State { groups } => {
                self.groups = groups;
                let ids: Vec<u64> = self.infos().map(|s| s.id).collect();
                self.locals.retain(|id, _| ids.contains(id));
                if !self.loaded {
                    self.selected = ids.first().copied();
                } else if self.selected.is_some_and(|id| !ids.contains(&id)) {
                    self.selected = None;
                }
                self.loaded = true;
            }
            ServerMsg::Created { id } => self.selected = Some(id),
            ServerMsg::Attached {
                id,
                cols,
                rows,
                replay,
            } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.terminal = Some(Terminal::new(cols, rows, &replay));
                    local.attaching = false;
                }
            }
            ServerMsg::Output { id, data } => {
                if let Some(terminal) = self.locals.get_mut(&id).and_then(|l| l.terminal.as_mut()) {
                    terminal.feed(&data);
                }
            }
            ServerMsg::AgentTranscript {
                id,
                items,
                busy,
                settings,
            } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.transcript = items;
                    local.busy = busy;
                    local.settings = settings;
                    local.rebuild();
                }
            }
            ServerMsg::AgentItem { id, index, item } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    match local.transcript.get_mut(index) {
                        Some(slot) => *slot = item,
                        None => local.transcript.push(item),
                    }
                    local.rebuild();
                }
            }
            ServerMsg::AgentSettings { id, settings } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.settings = settings;
                }
            }
            ServerMsg::AgentUsage { provider, windows } => {
                self.usage.insert(provider, windows);
            }
            ServerMsg::AgentModels { provider, models } => {
                self.models.insert(provider, models);
            }
            ServerMsg::AgentBusy { id, busy } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.busy = busy;
                    local.rebuild();
                }
            }
        }
    }

    // after a reconnect the server replays every terminal and transcript, so drop the stale local copies
    pub fn reset_terminals(&mut self) {
        for local in self.locals.values_mut() {
            local.terminal = None;
            local.attaching = false;
            local.opened = false;
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn usage(&self, provider: Provider) -> &[UsageWindow] {
        self.usage.get(&provider).map_or(&[], Vec::as_slice)
    }

    pub fn has_groups(&self) -> bool {
        !self.groups.is_empty()
    }

    pub fn selected(&mut self) -> Option<(&SessionInfo, &mut Local, &mut Vec<ClientMsg>, &Models)> {
        let id = self.selected?;
        let info = self
            .groups
            .iter()
            .flat_map(|g| &g.sessions)
            .find(|s| s.id == id)?;
        let local = self.locals.entry(id).or_default();
        Some((info, local, &mut self.outbox, &self.models))
    }

    // the id, folder and name of the group holding the selected session
    pub fn selected_group(&self) -> Option<(u64, Option<String>, String)> {
        let (g, _) = self.locate(self.selected?)?;
        let group = &self.groups[g];
        Some((group.id, group.path.clone(), group.name.clone()))
    }

    pub fn set_group_folder(&mut self, id: u64) {
        let current = self
            .groups
            .iter()
            .find(|g| g.id == id)
            .and_then(|g| g.path.clone());
        if let Some(path) = pick_folder(current.as_deref()) {
            self.outbox.push(ClientMsg::SetGroupPath {
                id,
                path: Some(path),
            });
        }
    }

    // new sessions land next to whatever is selected, so they stay in the current project
    pub fn create(&mut self, title: &str, kind: SessionKind) {
        let group = self
            .selected
            .and_then(|id| self.locate(id))
            .map(|(g, _)| self.groups[g].id);
        self.outbox.push(ClientMsg::CreateSession {
            group,
            title: title.to_owned(),
            kind,
        });
    }

    // a group is a project folder, named after the folder unless renamed later
    pub fn create_group_from_folder(&mut self) {
        let Some(path) = pick_folder(None) else {
            return;
        };
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        self.outbox.push(ClientMsg::CreateGroup {
            name,
            path: Some(path),
        });
    }

    fn infos(&self) -> impl Iterator<Item = &SessionInfo> {
        self.groups.iter().flat_map(|g| &g.sessions)
    }

    fn locate(&self, id: u64) -> Option<(usize, usize)> {
        self.groups.iter().enumerate().find_map(|(g, group)| {
            let i = group.sessions.iter().position(|s| s.id == id)?;
            Some((g, i))
        })
    }

    fn remove(&mut self, id: u64) {
        if self.selected == Some(id)
            && let Some((g, i)) = self.locate(id)
        {
            // select the row that moves into the gap, or the one above if it was the last
            let sessions = &self.groups[g].sessions;
            self.selected = sessions
                .get(i + 1)
                .or_else(|| i.checked_sub(1).and_then(|j| sessions.get(j)))
                .map(|s| s.id);
        }
        self.outbox.push(ClientMsg::DeleteSession { id });
    }

    fn is_noop(&self, id: u64, drop: Drop) -> bool {
        self.locate(id)
            .is_some_and(|(g, i)| g == drop.group && (drop.index == i || drop.index == i + 1))
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let p = theme::palette(ui);
        let dragging = self.drag.as_ref().map(|d| d.id);
        let pointer = ui.ctx().pointer_latest_pos();
        let mut actions = Vec::new();
        let mut drop: Option<(Drop, Option<f32>)> = None;
        let width = ui.available_width();

        ui.spacing_mut().item_spacing.y = 0.0;
        for (g, group) in self.groups.iter().enumerate() {
            if let Some((id, draft)) = &mut self.renaming
                && *id == group.id
            {
                let input = ui
                    .push_id(group.id, |ui| TextInput::new(draft).show(ui))
                    .inner;
                if !input.has_focus() && !input.lost_focus() {
                    input.request_focus();
                }
                let (enter, escape) =
                    ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                if input.lost_focus() {
                    if enter && !escape {
                        actions.push(Action::Rename(group.id, std::mem::take(draft)));
                    }
                    actions.push(Action::EndRename);
                }
            } else {
                let shown = ui
                    .push_id(group.id, |ui| {
                        GroupHeader::new(&group.name)
                            .detail(if group.path.is_some() {
                                ""
                            } else {
                                "no folder"
                            })
                            .open(group.open)
                            .drop_target(dragging.is_some())
                            .show(ui)
                    })
                    .inner;
                if shown.add.tip("New agent in this project").clicked() {
                    actions.push(Action::NewAgent(group.id));
                }
                let folder = group
                    .path
                    .as_deref()
                    .map(mux_core::display_path)
                    .unwrap_or_else(|| "No folder yet, right-click to choose one".to_owned());
                let header = shown.response.tip(folder);
                if header.clicked() {
                    actions.push(Action::Toggle(group.id));
                }
                header.context_menu(|ui| {
                    if MenuItem::new("Rename")
                        .icon(icons::PENCIL_SIMPLE)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::StartRename(group.id, group.name.clone()));
                        ui.close();
                    }
                    if MenuItem::new("Change folder…")
                        .icon(icons::FOLDER_OPEN)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::ChangeFolder(group.id));
                        ui.close();
                    }
                    if group.sessions.is_empty()
                        && MenuItem::new("Remove group")
                            .icon(icons::TRASH)
                            .danger()
                            .show(ui)
                            .clicked()
                    {
                        actions.push(Action::RemoveGroup(group.id));
                        ui.close();
                    }
                });
                if dragging.is_some() && header.contains_pointer() {
                    let index = if group.open { 0 } else { group.sessions.len() };
                    drop = Some((Drop { group: g, index }, None));
                }
            }

            let n = group.sessions.len() as f32;
            let full = n * (SessionRow::HEIGHT + ROW_GAP);
            let t = spring_bool(
                ui.ctx(),
                Id::unique("mux-group").with(group.id),
                group.open,
                Spring::SMOOTH,
            )
            .max(0.0);
            let (slot, _) = ui.allocate_exact_size(vec2(width, full * t), Sense::hover());
            if slot.height() > 0.5 {
                let mut rows = ui.new_child(
                    UiBuilder::new().max_rect(Rect::from_min_size(slot.min, vec2(width, full))),
                );
                rows.set_clip_rect(slot.intersect(ui.clip_rect()));
                rows.multiply_opacity(t.min(1.0));
                rows.spacing_mut().item_spacing.y = 0.0;
                for (i, session) in group.sessions.iter().enumerate() {
                    rows.add_space(ROW_GAP);
                    let is_dragged = dragging == Some(session.id);
                    let row = rows
                        .push_id(session.id, |ui| {
                            if is_dragged {
                                ui.multiply_opacity(0.35);
                            }
                            let (context, time) = (row_context(session), ago(session.updated));
                            let mut row =
                                SessionRow::new(&session.title, session.kind, session.status)
                                    .context(&context)
                                    .time(&time)
                                    .branch(group.branch.as_deref().unwrap_or_default())
                                    .selected(self.selected == Some(session.id));
                            if session.kind == SessionKind::Agent {
                                row = row.provider(session.provider);
                            }
                            row.show(ui)
                        })
                        .inner;
                    if row.clicked() {
                        actions.push(Action::Select(session.id));
                    }
                    if row.drag_started()
                        && let Some(pos) = row.interact_pointer_pos()
                    {
                        actions.push(Action::StartDrag(Drag {
                            id: session.id,
                            grab: pos - row.rect.min,
                            width: row.rect.width(),
                        }));
                    }
                    row.context_menu(|ui| {
                        if MenuItem::new("Delete")
                            .icon(icons::TRASH)
                            .danger()
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::Remove(session.id));
                            ui.close();
                        }
                    });

                    let zone = row.rect.expand2(vec2(0.0, ROW_GAP / 2.0));
                    if dragging.is_some()
                        && let Some(pos) = pointer
                        && zone.contains(pos)
                        && slot.contains(pos)
                    {
                        let before = pos.y < row.rect.center().y;
                        let index = if before { i } else { i + 1 };
                        let y = if before { zone.top() } else { zone.bottom() };
                        drop = Some((Drop { group: g, index }, Some(y)));
                    }
                }
            }
            ui.add_space(GROUP_GAP);
        }

        if let Some(id) = dragging {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            let drop = drop.filter(|(d, _)| !self.is_noop(id, *d));
            if let Some((_, Some(y))) = drop {
                let x = ui.max_rect().x_range();
                ui.painter()
                    .hline(x.min + 4.0..=x.max - 4.0, y, Stroke::new(2.0, p.accent));
                ui.painter()
                    .circle(pos2(x.min + 4.0, y), 3.0, p.bg, Stroke::new(2.0, p.accent));
            }
            self.ghost(ui, pointer);

            let (released, cancelled) = ui.input_mut(|i| {
                (
                    i.pointer.any_released(),
                    i.consume_key(Modifiers::NONE, Key::Escape),
                )
            });
            if released || cancelled {
                if let (false, Some((d, _))) = (cancelled, drop) {
                    self.outbox.push(ClientMsg::MoveSession {
                        id,
                        group: self.groups[d.group].id,
                        index: d.index,
                    });
                }
                self.drag = None;
            }
        }

        for action in actions {
            match action {
                Action::Toggle(id) => self.outbox.push(ClientMsg::ToggleGroup { id }),
                Action::NewAgent(group) => self.outbox.push(ClientMsg::CreateSession {
                    group: Some(group),
                    title: "New agent".to_owned(),
                    kind: SessionKind::Agent,
                }),
                Action::StartRename(id, name) => self.renaming = Some((id, name)),
                Action::Rename(id, name) => self.outbox.push(ClientMsg::RenameGroup { id, name }),
                Action::EndRename => self.renaming = None,
                Action::ChangeFolder(id) => self.set_group_folder(id),
                Action::RemoveGroup(id) => self.outbox.push(ClientMsg::DeleteGroup { id }),
                Action::Select(id) => self.selected = Some(id),
                Action::Remove(id) => self.remove(id),
                Action::StartDrag(drag) => self.drag = Some(drag),
            }
        }
    }

    // a lifted copy of the row that follows the pointer while dragging
    fn ghost(&self, ui: &egui::Ui, pointer: Option<egui::Pos2>) {
        let (Some(drag), Some(pos)) = (&self.drag, pointer) else {
            return;
        };
        let Some(session) = self.infos().find(|s| s.id == drag.id) else {
            return;
        };
        let p = theme::palette(ui);
        egui::Area::new(Id::unique("mux-session-ghost"))
            .order(Order::Tooltip)
            .fixed_pos(pos - drag.grab)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                Frame::new()
                    .fill(p.elevated)
                    .stroke(Stroke::new(1.0, p.border_strong))
                    .corner_radius(CornerRadius::same(radius::MD))
                    .shadow(ui.visuals().popup_shadow)
                    .show(ui, |ui| {
                        ui.set_width(drag.width);
                        SessionRow::new(&session.title, session.kind, session.status)
                            .context(&row_context(session))
                            .time(&ago(session.updated))
                            .show(ui);
                    });
            });
    }
}

// the native dialog blocks this frame, which is fine for a one-off pick and required on macos
fn pick_folder(start: Option<&str>) -> Option<String> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose a project folder");
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    Some(dialog.pick_folder()?.to_string_lossy().into_owned())
}

// the line above a row's title: what needs attention first, otherwise where it is
fn row_context(session: &SessionInfo) -> String {
    match (session.status, session.kind) {
        (SessionStatus::Waiting, _) => "Needs your input".to_owned(),
        (SessionStatus::Idle, _) => "Not started".to_owned(),
        (SessionStatus::Running, SessionKind::Agent) => "Working…".to_owned(),
        (SessionStatus::Done, SessionKind::Agent) => "Finished".to_owned(),
        (SessionStatus::Failed, _) if session.meta.is_empty() => "Failed".to_owned(),
        _ => session.meta.clone(),
    }
}

fn ago(updated: u64) -> String {
    if updated == 0 {
        return String::new();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(updated);
    match now.saturating_sub(updated) {
        s if s < 60 => "now".to_owned(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

fn chat_items(transcript: &[AgentItem], busy: bool) -> (Vec<ChatItem>, Vec<Option<u64>>) {
    let mut items = Vec::new();
    let mut approvals = Vec::new();
    for entry in transcript {
        let mut approval = None;
        match entry {
            AgentItem::User { text } => items.push(ChatItem::User(text.clone())),
            // a reply that has started but not streamed any words yet shows as thinking instead
            AgentItem::Assistant { text } if text.trim().is_empty() => continue,
            AgentItem::Assistant { text } => items.push(ChatItem::Assistant(text.clone())),
            AgentItem::Command {
                command,
                output,
                status,
                ..
            } => items.push(ChatItem::Tool(ToolCall {
                kind: ToolKind::Run,
                target: shell_body(command),
                status: tool_status(*status),
                output: (!output.is_empty()).then(|| output.clone()),
            })),
            AgentItem::FileChange { changes, .. } => {
                for change in changes {
                    items.push(ChatItem::Diff(FileDiff {
                        path: change.path.clone(),
                        lines: diff_lines(&change.diff),
                    }));
                    approvals.push(None);
                }
                continue;
            }
            AgentItem::Approval {
                id,
                title,
                detail,
                decision,
            } => {
                approval = Some(*id);
                items.push(ChatItem::Permission(PermissionRequest {
                    title: title.clone(),
                    detail: (!detail.is_empty()).then(|| shell_body(detail)),
                    decision: decision.map(|d| match d {
                        wire::Decision::Allow => Decision::Allow,
                        wire::Decision::AlwaysAllow => Decision::AlwaysAllow,
                        wire::Decision::Deny => Decision::Deny,
                    }),
                }));
            }
            AgentItem::Tool {
                name,
                detail,
                output,
                status,
            } => items.push(ChatItem::Tool(ToolCall {
                kind: match name.as_str() {
                    "Read" | "NotebookRead" => ToolKind::Read,
                    "Grep" | "Glob" | "WebSearch" | "WebFetch" | "LS" => ToolKind::Search,
                    _ => ToolKind::Run,
                },
                target: if detail.is_empty() {
                    name.clone()
                } else {
                    detail.clone()
                },
                status: tool_status(*status),
                output: (!output.is_empty()).then(|| output.clone()),
            })),
            AgentItem::Error { message } => {
                items.push(ChatItem::Assistant(format!("**Error:** {message}")))
            }
        }
        approvals.push(approval);
    }
    chat::set_thinking(&mut items, busy);
    approvals.resize(items.len(), None);
    (items, approvals)
}

fn tool_status(status: ItemStatus) -> ToolStatus {
    match status {
        ItemStatus::Running => ToolStatus::Running,
        ItemStatus::Done => ToolStatus::Done,
        ItemStatus::Failed | ItemStatus::Declined => ToolStatus::Failed,
    }
}

// codex reports commands as `/usr/bin/bash -lc "..."`, and the quoted part is what people want to read
fn shell_body(command: &str) -> String {
    let unwrapped = ["-lc ", "-c "].iter().find_map(|flag| {
        let (shell, body) = command.split_once(flag)?;
        let name = shell.trim().rsplit('/').next()?;
        ["bash", "sh", "zsh", "fish"]
            .contains(&name)
            .then_some(body.trim())
    });
    let Some(body) = unwrapped else {
        return command.to_owned();
    };
    if let Some(inner) = body.strip_prefix('"').and_then(|b| b.strip_suffix('"')) {
        inner.replace("\\\"", "\"").replace("\\\\", "\\")
    } else if let Some(inner) = body.strip_prefix('\'').and_then(|b| b.strip_suffix('\'')) {
        inner.to_owned()
    } else {
        body.to_owned()
    }
}

fn diff_lines(diff: &str) -> Vec<DiffLine> {
    let headers = ["--- ", "+++ ", "@@", "diff ", "index "];
    let lines = diff
        .lines()
        .filter(|l| !headers.iter().any(|h| l.starts_with(h)));
    // a brand new file can arrive as plain contents rather than a patch
    let is_patch = diff.lines().any(|l| l.starts_with("@@"));
    lines
        .map(|line| {
            let (kind, text) = match line.chars().next() {
                _ if !is_patch => (DiffKind::Add, line),
                Some('+') => (DiffKind::Add, &line[1..]),
                Some('-') => (DiffKind::Remove, &line[1..]),
                Some(' ') => (DiffKind::Context, &line[1..]),
                _ => (DiffKind::Context, line),
            };
            DiffLine {
                kind,
                text: text.to_owned(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_shell_commands() {
        assert_eq!(
            shell_body("/usr/bin/bash -lc \"printf 'hi\\\\n' > hello.txt\""),
            "printf 'hi\\n' > hello.txt"
        );
        assert_eq!(shell_body("/bin/zsh -c 'ls -la'"), "ls -la");
        assert_eq!(shell_body("cargo test -c foo"), "cargo test -c foo");
    }

    #[test]
    fn approvals_line_up_with_chat_items() {
        let transcript = vec![
            AgentItem::User { text: "hi".into() },
            AgentItem::FileChange {
                changes: vec![
                    wire::FileChange {
                        path: "a".into(),
                        diff: "@@ -1 +1 @@\n-old\n+new".into(),
                    },
                    wire::FileChange {
                        path: "b".into(),
                        diff: "fresh file".into(),
                    },
                ],
                status: ItemStatus::Done,
            },
            AgentItem::Approval {
                id: 9,
                title: "Run?".into(),
                detail: String::new(),
                decision: None,
            },
        ];
        let (items, approvals) = chat_items(&transcript, true);
        assert_eq!(items.len(), 5, "user, two diffs, approval, thinking");
        assert_eq!(approvals, vec![None, None, None, Some(9), None]);
        let ChatItem::Diff(first) = &items[1] else {
            panic!()
        };
        assert_eq!(first.lines.len(), 2);
        assert_eq!(first.lines[0].kind, DiffKind::Remove);
        let ChatItem::Diff(second) = &items[2] else {
            panic!()
        };
        assert_eq!(second.lines[0].kind, DiffKind::Add);
    }
}
