use std::collections::HashMap;

use eframe::egui::{
    self, CornerRadius, CursorIcon, Frame, Id, Key, Modifiers, Order, Rect, Sense, Stroke,
    UiBuilder, Vec2, pos2, vec2,
};
use mux_core::SessionKind;
use mux_core::protocol::{ClientMsg, GroupInfo, ServerMsg, SessionInfo};
use mux_ui::chat::{self, ChatAction, ChatItem, Decision, ToolCall, ToolKind, ToolStatus};
use mux_ui::motion::{Spring, spring_bool};
use mux_ui::theme::{self, radius};
use mux_ui::{GroupHeader, MenuItem, SessionRow, TextInput, TooltipExt, icons};

use crate::terminal::Terminal;

const ROW_GAP: f32 = 2.0;
const GROUP_GAP: f32 = 6.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Chat,
    Terminal,
}

// per session state that only this window cares about
pub struct Local {
    pub view: View,
    pub chat: Vec<ChatItem>,
    pub draft: String,
    pub thinking: bool,
    pub terminal: Option<Terminal>,
    pub attaching: bool,
}

impl Local {
    fn new(chat: Vec<ChatItem>) -> Self {
        Self {
            view: View::Chat,
            chat,
            draft: String::new(),
            thinking: false,
            terminal: None,
            attaching: false,
        }
    }

    // stands in for the agent until the server forwards these, so every chat state can be tried
    pub fn chat_action(&mut self, action: ChatAction) {
        match action {
            ChatAction::Send(text) => {
                self.chat.push(ChatItem::User(text));
                self.thinking = true;
            }
            ChatAction::Stop => {
                for item in &mut self.chat {
                    if let ChatItem::Tool(tool) = item
                        && tool.status == ToolStatus::Running
                    {
                        tool.status = ToolStatus::Failed;
                    }
                }
                self.thinking = false;
            }
            ChatAction::Decide(i, decision) => {
                let Some(ChatItem::Permission(request)) = self.chat.get_mut(i) else {
                    return;
                };
                request.decision = Some(decision);
                if decision != Decision::Deny {
                    let command = request.detail.clone().unwrap_or_default();
                    self.chat.push(ChatItem::Tool(ToolCall {
                        kind: ToolKind::Run,
                        target: command,
                        status: ToolStatus::Running,
                        output: None,
                    }));
                    self.thinking = true;
                }
            }
        }
        chat::set_thinking(&mut self.chat, self.thinking);
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
}

#[derive(Default)]
pub struct Sessions {
    groups: Vec<GroupInfo>,
    locals: HashMap<u64, Local>,
    selected: Option<u64>,
    drag: Option<Drag>,
    loaded: bool,
    sampled: bool,
    // the group whose name is being edited inline, with the draft name
    renaming: Option<(u64, String)>,
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
        }
    }

    // after a reconnect the server replays every terminal, so drop the stale local copies
    pub fn reset_terminals(&mut self) {
        for local in self.locals.values_mut() {
            local.terminal = None;
            local.attaching = false;
        }
    }

    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn selected(&mut self) -> Option<(&SessionInfo, &mut Local, &mut Vec<ClientMsg>)> {
        let id = self.selected?;
        let info = self
            .groups
            .iter()
            .flat_map(|g| &g.sessions)
            .find(|s| s.id == id)?;
        let local = self.locals.entry(id).or_insert_with(|| {
            // the placeholder conversation goes to the first agent opened, until agents are wired up
            let sample = info.kind == SessionKind::Agent && !self.sampled;
            self.sampled |= sample;
            Local::new(if sample { chat::sample() } else { Vec::new() })
        });
        Some((info, local, &mut self.outbox))
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
                let header = ui
                    .push_id(group.id, |ui| {
                        GroupHeader::new(&group.name, group.sessions.len())
                            .open(group.open)
                            .drop_target(dragging.is_some())
                            .show(ui)
                    })
                    .inner;
                let folder = group
                    .path
                    .as_deref()
                    .map(mux_core::display_path)
                    .unwrap_or_else(|| "~ (no folder set)".to_owned());
                let header = header.tip(folder);
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
                            SessionRow::new(&session.title, session.kind, session.status)
                                .meta(&session.meta)
                                .selected(self.selected == Some(session.id))
                                .show(ui)
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
                Action::StartRename(id, name) => self.renaming = Some((id, name)),
                Action::Rename(id, name) => self.outbox.push(ClientMsg::RenameGroup { id, name }),
                Action::EndRename => self.renaming = None,
                Action::ChangeFolder(id) => {
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
                            .meta(&session.meta)
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
