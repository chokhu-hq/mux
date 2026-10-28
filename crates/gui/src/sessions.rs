use std::collections::{HashMap, HashSet};

use eframe::egui::{
    self, CursorIcon, Id, Key, Modifiers, Order, Rect, Sense, Stroke, UiBuilder, Vec2, pos2, vec2,
};
use mux_core::protocol::{
    self as wire, AgentItem, AgentSettings, ClientMsg, ContextUsage, GroupInfo, ItemStatus,
    ModelOption, ServerMsg, SessionInfo, Skill, UsageDay, UsageWindow,
};
use mux_core::{Provider, SessionKind, SessionStatus};
use mux_ui::chat::{
    self, ChatItem, Decision, DiffKind, DiffLine, FileDiff, PermissionRequest, ToolCall, ToolKind,
    ToolStatus,
};
use mux_ui::motion::{Spring, spring_bool};
use mux_ui::theme;
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
    pub context: Option<ContextUsage>,
    // the skills the agent can be pointed at with `$name`, offered alongside mux's commands
    pub skills: Vec<Skill>,
    // messages waiting for the agent to finish, and whether a stop is holding them
    pub queue: Vec<wire::Queued>,
    pub held: bool,
    // files picked or dropped for the next message
    pub attachments: Vec<String>,
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
        (self.chat, self.approvals) =
            chat_items(&self.transcript, self.busy, self.settings.provider);
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
    StartGroupDrag(Drag),
    NewAgent(u64),
}

pub enum Event {
    Finished,
    Waiting,
    Failed,
}

pub struct Alert {
    pub id: u64,
    pub title: String,
    pub event: Event,
}

#[derive(Default)]
pub struct Sessions {
    groups: Vec<GroupInfo>,
    locals: HashMap<u64, Local>,
    selected: Option<u64>,
    // a session was picked since the last check, which takes the window off the settings page
    picked: bool,
    drag: Option<Drag>,
    // a group being dragged to a new place in the sidebar
    group_drag: Option<Drag>,
    loaded: bool,
    // the group whose name is being edited inline, with the draft name
    renaming: Option<(u64, String)>,
    // the models each agent cli offers, sent by the server once it has asked
    models: Models,
    usage: HashMap<Provider, Vec<UsageWindow>>,
    // the last month of token use and which day is today, once the server has read the clis' history
    tokens: Option<(i64, Vec<UsageDay>)>,
    // sessions where an agent finished or stopped to ask something, since they were last looked at
    unread: HashSet<u64>,
    // agents that just finished, failed or started waiting, for the app to point out
    alerts: Vec<Alert>,
    pub outbox: Vec<ClientMsg>,
    // what each session asked for should start with, in the order the server confirms them
    creating: std::collections::VecDeque<Option<mux_core::protocol::AgentSettings>>,
    // the settings a new agent starts with, chosen in settings
    pub agent_defaults: mux_core::protocol::AgentSettings,
}

impl Sessions {
    // what agents did since the last call, marking each unread unless it's the one being looked at
    pub fn take_alerts(&mut self, looking: Option<u64>) -> Vec<Alert> {
        let alerts = std::mem::take(&mut self.alerts);
        for alert in &alerts {
            if looking != Some(alert.id) {
                self.unread.insert(alert.id);
            }
        }
        if let Some(id) = looking {
            self.unread.remove(&id);
        }
        alerts
    }

    // every session in sidebar order, for the palette and the number shortcuts
    pub fn destinations(&self) -> Vec<mux_ui::Destination> {
        self.groups
            .iter()
            .flat_map(|g| g.sessions.iter().map(move |s| (g, s)))
            .map(|(group, session)| mux_ui::Destination {
                id: session.id,
                title: session.title.clone(),
                detail: group.name.clone(),
                icon: match session.kind {
                    SessionKind::Agent => icons::ROBOT,
                    SessionKind::Shell => icons::TERMINAL,
                    SessionKind::Task => icons::PLAY,
                },
            })
            .collect()
    }

    pub fn select(&mut self, id: u64) {
        self.selected = Some(id);
        self.picked = true;
    }

    pub fn selected_id(&self) -> Option<u64> {
        self.selected
    }

    pub fn take_picked(&mut self) -> bool {
        std::mem::take(&mut self.picked)
    }

    pub fn apply(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome { .. } => {}
            ServerMsg::State { groups } => {
                let before: HashMap<u64, SessionStatus> =
                    self.infos().map(|s| (s.id, s.status)).collect();
                self.groups = groups;
                // only a change seen while connected counts, so starting up doesn't announce every session
                if self.loaded {
                    let alerts: Vec<Alert> = self
                        .infos()
                        .filter(|s| s.kind == SessionKind::Agent)
                        .filter_map(|s| {
                            let was = *before.get(&s.id)?;
                            let event = match (was, s.status) {
                                (SessionStatus::Running, SessionStatus::Done) => Event::Finished,
                                (SessionStatus::Running, SessionStatus::Waiting) => Event::Waiting,
                                (SessionStatus::Running, SessionStatus::Failed) => Event::Failed,
                                _ => return None,
                            };
                            Some(Alert {
                                id: s.id,
                                title: s.title.clone(),
                                event,
                            })
                        })
                        .collect();
                    self.alerts.extend(alerts);
                }
                let ids: Vec<u64> = self.infos().map(|s| s.id).collect();
                self.locals.retain(|id, _| ids.contains(id));
                if !self.loaded {
                    self.selected = ids.first().copied();
                } else if self.selected.is_some_and(|id| !ids.contains(&id)) {
                    self.selected = None;
                }
                self.loaded = true;
            }
            ServerMsg::Created { id } => {
                // a new agent takes the defaults from settings before its first message
                if let Some(Some(settings)) = self.creating.pop_front() {
                    self.outbox.push(ClientMsg::AgentConfigure { id, settings });
                }
                self.selected = Some(id);
                self.picked = true;
            }
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
            ServerMsg::AgentQueue { id, queue, held } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.queue = queue;
                    local.held = held;
                }
            }
            ServerMsg::AgentSkills { id, skills } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.skills = skills;
                }
            }
            ServerMsg::AgentTranscript {
                id,
                items,
                busy,
                settings,
                context,
            } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.transcript = items;
                    local.busy = busy;
                    local.settings = settings;
                    local.context = context;
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
            ServerMsg::AgentContext { id, context } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.context = Some(context);
                }
            }
            ServerMsg::AgentUsage { provider, windows } => {
                self.usage.insert(provider, windows);
            }
            ServerMsg::TokenHistory { today, days } => self.tokens = Some((today, days)),
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

    pub fn tokens(&self) -> Option<&(i64, Vec<UsageDay>)> {
        self.tokens.as_ref()
    }

    pub fn models(&self) -> &Models {
        &self.models
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
    // the branch the selected session works on, its own worktree's or else its project's
    pub fn selected_branch(&self) -> Option<String> {
        let (g, i) = self.locate(self.selected?)?;
        let group = &self.groups[g];
        group.sessions[i].branch.clone().or(group.branch.clone())
    }

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
        self.request(group, title, kind);
    }

    // every new session goes through here, so the server's confirmations line up with what was asked for
    fn request(&mut self, group: Option<u64>, title: &str, kind: SessionKind) {
        let settings = (kind == SessionKind::Agent).then(|| self.agent_defaults.clone());
        self.creating.push_back(settings);
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
        let group_dragging = self.group_drag.as_ref().map(|d| d.id);
        let pointer = ui.ctx().pointer_latest_pos();
        let mut actions = Vec::new();
        let mut drop: Option<(Drop, Option<f32>)> = None;
        let width = ui.available_width();
        // where each group's header and rows sit, to find the gap a dragged group would land in
        let mut blocks = Vec::new();

        ui.spacing_mut().item_spacing.y = 0.0;
        for (g, group) in self.groups.iter().enumerate() {
            let block_top = ui.cursor().top();
            let opacity = ui.opacity();
            if group_dragging == Some(group.id) {
                ui.multiply_opacity(0.35);
            }
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
                if header.drag_started()
                    && dragging.is_none()
                    && let Some(pos) = header.interact_pointer_pos()
                {
                    actions.push(Action::StartGroupDrag(Drag {
                        id: group.id,
                        grab: pos - header.rect.min,
                        width: header.rect.width(),
                    }));
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
                                    .branch(
                                        session
                                            .branch
                                            .as_deref()
                                            .or(group.branch.as_deref())
                                            .unwrap_or_default(),
                                    )
                                    .selected(self.selected == Some(session.id))
                                    .unread(self.unread.contains(&session.id));
                            let model = model_name(&self.models, session);
                            if session.kind == SessionKind::Agent {
                                row = row.provider(session.provider).model(&model);
                            }
                            if let Some(summary) = &session.summary {
                                row = row.changes(summary.added, summary.removed);
                            }
                            let branch = session.branch.as_deref().or(group.branch.as_deref());
                            // the card waits until the pointer settles, and never shows mid drag
                            let row = row.show(ui);
                            if dragging.is_some() {
                                row
                            } else {
                                row.tip_ui(|ui| session_card(ui, session, group, branch, &model))
                            }
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
            blocks.push((block_top, ui.cursor().top()));
            ui.set_opacity(opacity);
            ui.add_space(GROUP_GAP);
        }

        if let Some(id) = group_dragging {
            self.drop_group(ui, id, &blocks, pointer);
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
                Action::NewAgent(group) => {
                    self.request(Some(group), "New agent", SessionKind::Agent)
                }
                Action::StartRename(id, name) => self.renaming = Some((id, name)),
                Action::Rename(id, name) => self.outbox.push(ClientMsg::RenameGroup { id, name }),
                Action::EndRename => self.renaming = None,
                Action::ChangeFolder(id) => self.set_group_folder(id),
                Action::RemoveGroup(id) => self.outbox.push(ClientMsg::DeleteGroup { id }),
                Action::Select(id) => {
                    self.selected = Some(id);
                    self.picked = true;
                }
                Action::Remove(id) => self.remove(id),
                Action::StartDrag(drag) => self.drag = Some(drag),
                Action::StartGroupDrag(drag) => self.group_drag = Some(drag),
            }
        }
    }

    // shows where a dragged group would land, and moves it there on release
    fn drop_group(
        &mut self,
        ui: &egui::Ui,
        id: u64,
        blocks: &[(f32, f32)],
        pointer: Option<egui::Pos2>,
    ) {
        let p = theme::palette(ui);
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        let from = self.groups.iter().position(|g| g.id == id);
        let index = pointer.and_then(|pos| group_gap(blocks, pos.y, from));
        if let Some(index) = index {
            let y = match index {
                0 => blocks.first().map_or(0.0, |b| b.0) - GROUP_GAP / 2.0,
                i => blocks[i - 1].1 + GROUP_GAP / 2.0,
            };
            let x = ui.max_rect().x_range();
            ui.painter()
                .hline(x.min + 4.0..=x.max - 4.0, y, Stroke::new(2.0, p.accent));
            ui.painter()
                .circle(pos2(x.min + 4.0, y), 3.0, p.bg, Stroke::new(2.0, p.accent));
        }
        if let (Some(drag), Some(pos), Some(group)) = (
            &self.group_drag,
            pointer,
            self.groups.iter().find(|g| g.id == id),
        ) {
            egui::Area::new(Id::unique("mux-group-ghost"))
                .order(Order::Tooltip)
                .fixed_pos(pos - drag.grab)
                .interactable(false)
                .show(ui.ctx(), |ui| {
                    theme::floating(ui).show(ui, |ui| {
                        ui.set_width(drag.width);
                        GroupHeader::new(&group.name).open(group.open).show(ui);
                    });
                });
        }
        let (released, cancelled) = ui.input_mut(|i| {
            (
                i.pointer.any_released(),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if released || cancelled {
            if let (false, Some(index)) = (cancelled, index) {
                // moved here too so the sidebar doesn't jump back until the server agrees
                if let Some(from) = from {
                    let group = self.groups.remove(from);
                    let at = if from < index { index - 1 } else { index };
                    self.groups.insert(at.min(self.groups.len()), group);
                }
                self.outbox.push(ClientMsg::MoveGroup { id, index });
            }
            self.group_drag = None;
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
        egui::Area::new(Id::unique("mux-session-ghost"))
            .order(Order::Tooltip)
            .fixed_pos(pos - drag.grab)
            .interactable(false)
            .show(ui.ctx(), |ui| {
                theme::floating(ui).show(ui, |ui| {
                    ui.set_width(drag.width);
                    SessionRow::new(&session.title, session.kind, session.status)
                        .context(&row_context(session))
                        .time(&ago(session.updated))
                        .show(ui);
                });
            });
    }
}

// the gap past every group whose middle is above the pointer, unless that leaves the group where it is
fn group_gap(blocks: &[(f32, f32)], y: f32, from: Option<usize>) -> Option<usize> {
    let gap = blocks
        .iter()
        .filter(|(top, bottom)| (top + bottom) / 2.0 < y)
        .count();
    from.is_none_or(|from| gap != from && gap != from + 1)
        .then_some(gap)
}

// the native dialog blocks this frame, which is fine for a one-off pick and required on macos
pub fn pick_files(start: Option<&str>) -> Vec<String> {
    let mut dialog = rfd::FileDialog::new().set_title("Attach files");
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog
        .pick_files()
        .unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

// the dialog shows this computer's folders, and the server is told the folder as it names it
fn pick_folder(start: Option<&str>) -> Option<String> {
    let mut dialog = rfd::FileDialog::new().set_title("Choose a project folder");
    if let Some(start) = start {
        dialog = dialog.set_directory(mux_core::paths::local(start));
    }
    let picked = dialog.pick_folder()?.to_string_lossy().into_owned();
    Some(mux_core::paths::remote(&picked))
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

// the model an agent runs as people know it, falling back to its id and then to the cli's default
fn model_name(models: &Models, session: &SessionInfo) -> String {
    if session.kind != SessionKind::Agent {
        return String::new();
    }
    let list = models.get(&session.provider);
    let id = session
        .summary
        .as_ref()
        .map(|s| s.model.as_str())
        .unwrap_or_default();
    list.and_then(|list| {
        list.iter()
            .find(|m| !id.is_empty() && m.id == id)
            .or_else(|| list.iter().find(|m| id.is_empty() && m.is_default))
    })
    .map_or_else(|| id.to_owned(), |m| m.name.clone())
}

// everything worth knowing about a session at a glance, shown when its row is hovered
fn session_card(
    ui: &mut egui::Ui,
    session: &SessionInfo,
    group: &GroupInfo,
    branch: Option<&str>,
    model: &str,
) {
    let p = theme::palette(ui);
    ui.set_width(300.0);
    ui.spacing_mut().item_spacing.y = 0.0;
    ui.spacing_mut().interact_size.y = 0.0;
    ui.add(
        egui::Label::new(
            egui::RichText::new(&session.title)
                .font(theme::font::medium(theme::text::MD + 1.0))
                .color(p.text),
        )
        .wrap(),
    );
    ui.add_space(theme::space::XS);
    let (color, label) = match session.status {
        SessionStatus::Running => (p.accent, "Working"),
        SessionStatus::Waiting => (p.warning, "Needs your input"),
        SessionStatus::Failed => (p.danger, "Failed"),
        SessionStatus::Done if session.kind == SessionKind::Agent => (p.success, "Finished"),
        SessionStatus::Done => (p.text_faint, "Exited"),
        SessionStatus::Idle => (p.text_faint, "Not started"),
    };
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::space::SM;
        let (dot, _) = ui.allocate_exact_size(vec2(8.0, 14.0), Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.5, color);
        let when = ago(session.updated);
        let text = if when.is_empty() || when == "now" {
            label.to_owned()
        } else {
            format!("{label} · {when} ago")
        };
        ui.label(
            egui::RichText::new(text)
                .font(theme::font::regular(theme::text::SM + 1.0))
                .color(p.text_muted),
        );
    });
    ui.add_space(theme::space::MD);
    let line = ui.available_rect_before_wrap();
    ui.painter()
        .hline(line.x_range(), line.top(), Stroke::new(1.0, p.border));
    ui.add_space(theme::space::SM);

    let mut facts: Vec<(&str, String)> = Vec::new();
    let folder = session
        .worktree
        .as_deref()
        .or(group.path.as_deref())
        .map(mux_core::display_path)
        .unwrap_or_else(|| "No folder".to_owned());
    facts.push(("Project", group.name.clone()));
    facts.push((
        if session.worktree.is_some() {
            "Worktree"
        } else {
            "Folder"
        },
        folder,
    ));
    if let Some(branch) = branch {
        facts.push(("Branch", branch.to_owned()));
    }
    match session.kind {
        SessionKind::Agent => {
            let agent = session.provider.label();
            facts.push((
                "Agent",
                if model.is_empty() {
                    agent.to_owned()
                } else {
                    format!("{agent} · {model}")
                },
            ));
            if let Some(summary) = &session.summary {
                facts.push(("Messages", summary.messages.to_string()));
                if summary.files > 0 {
                    let files = if summary.files == 1 { "file" } else { "files" };
                    facts.push((
                        "Changes",
                        format!(
                            "{} {files}, +{} −{}",
                            summary.files, summary.added, summary.removed
                        ),
                    ));
                }
                if let Some(context) = summary.context.filter(|c| c.window > 0) {
                    let share = context.used as f32 / context.window as f32;
                    facts.push((
                        "Context",
                        format!(
                            "{} of {} · {}%",
                            mux_ui::short_tokens(context.used),
                            mux_ui::short_tokens(context.window),
                            (share * 100.0).round() as u32
                        ),
                    ));
                }
            }
        }
        SessionKind::Shell | SessionKind::Task if !session.meta.is_empty() => {
            let label = if session.kind == SessionKind::Task {
                "Command"
            } else {
                "Running in"
            };
            facts.push((label, session.meta.clone()));
        }
        _ => {}
    }
    for (label, value) in facts {
        ui.allocate_ui_with_layout(
            vec2(ui.available_width(), 22.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(
                    egui::RichText::new(label)
                        .font(theme::font::regular(theme::text::SM + 1.0))
                        .color(p.text_faint),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut job = egui::text::LayoutJob::simple_singleline(
                        value,
                        theme::font::regular(theme::text::SM + 1.0),
                        p.text,
                    );
                    job.wrap = egui::text::TextWrapping {
                        max_width: 200.0,
                        max_rows: 1,
                        break_anywhere: true,
                        overflow_character: Some('…'),
                    };
                    ui.label(job);
                });
            },
        );
    }

    let preview = session
        .summary
        .as_ref()
        .map(|s| s.preview.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .replace("**", "")
        .replace('`', "");
    if !preview.is_empty() {
        ui.add_space(theme::space::SM);
        let line = ui.available_rect_before_wrap();
        ui.painter()
            .hline(line.x_range(), line.top(), Stroke::new(1.0, p.border));
        ui.add_space(theme::space::SM);
        let mut job = egui::text::LayoutJob::simple(
            preview,
            theme::font::regular(theme::text::SM + 1.0),
            p.text_muted,
            300.0,
        );
        job.wrap.max_rows = 3;
        job.wrap.overflow_character = Some('…');
        ui.label(job);
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

fn chat_items(
    transcript: &[AgentItem],
    busy: bool,
    provider: Provider,
) -> (Vec<ChatItem>, Vec<Option<u64>>) {
    let mut items = Vec::new();
    let mut approvals = Vec::new();
    for (position, entry) in transcript.iter().enumerate() {
        let mut approval = None;
        match entry {
            AgentItem::User {
                text,
                attachments,
                queued,
                ..
            } => items.push(ChatItem::User {
                text: text.clone(),
                attachments: attachments
                    .iter()
                    .map(|path| mux_core::paths::local(path))
                    .collect(),
                queued: *queued,
            }),
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
            AgentItem::Subagent {
                task,
                kind,
                background,
                status,
                activity,
                steps,
                result,
                tokens,
                ..
            } => items.push(ChatItem::Subagent(chat::Subagent {
                provider,
                task: task.clone(),
                kind: kind.clone(),
                status: tool_status(*status),
                background: *background,
                activity: activity.clone(),
                steps: steps
                    .iter()
                    .map(|step| ToolCall {
                        kind: tool_kind(&step.name),
                        target: match step.name.as_str() {
                            "Bash" => shell_body(&step.detail),
                            _ if step.detail.is_empty() => step.name.clone(),
                            _ => step.detail.clone(),
                        },
                        status: tool_status(step.status),
                        output: None,
                    })
                    .collect(),
                result: result.clone(),
                tokens: *tokens,
            })),
            AgentItem::Tool {
                name,
                detail,
                output,
                status,
            } => items.push(ChatItem::Tool(ToolCall {
                kind: tool_kind(name),
                target: if detail.is_empty() {
                    name.clone()
                } else {
                    detail.clone()
                },
                status: tool_status(*status),
                output: (!output.is_empty()).then(|| output.clone()),
            })),
            AgentItem::Compaction { status } => {
                items.push(ChatItem::Compaction(tool_status(*status)))
            }
            AgentItem::Question {
                id,
                questions,
                answers,
            } => {
                approval = Some(*id);
                items.push(ChatItem::Question {
                    questions: questions.clone(),
                    answers: answers.clone(),
                });
            }
            AgentItem::Plan { text } => {
                // only the latest plan, not yet replied to, can still be carried out
                let replied = transcript[position + 1..]
                    .iter()
                    .any(|i| matches!(i, AgentItem::User { .. } | AgentItem::Plan { .. }));
                items.push(ChatItem::Plan {
                    text: text.clone(),
                    actionable: !replied && !busy,
                });
            }
            AgentItem::Todos { steps } => items.push(ChatItem::Todos(steps.clone())),
            AgentItem::Error { message } => {
                items.push(ChatItem::Assistant(format!("**Error:** {message}")))
            }
        }
        approvals.push(approval);
    }
    approvals.resize(items.len(), None);
    let (mut items, approvals) = summarise_changes(items, approvals, busy);
    chat::set_thinking(&mut items, busy);
    (items, approvals)
}

// a finished turn that touched several files ends with one line that opens them all for review
fn summarise_changes(
    items: Vec<ChatItem>,
    approvals: Vec<Option<u64>>,
    busy: bool,
) -> (Vec<ChatItem>, Vec<Option<u64>>) {
    let mut out = Vec::with_capacity(items.len());
    let mut out_approvals = Vec::with_capacity(items.len());
    // the first edit card's place in `out`, the files touched and the lines added and removed
    let mut turn: Option<(usize, Vec<String>, usize, usize)> = None;
    let close = |turn: &mut Option<(usize, Vec<String>, usize, usize)>,
                 out: &mut Vec<ChatItem>,
                 out_approvals: &mut Vec<Option<u64>>| {
        if let Some((first, files, added, removed)) = turn.take()
            && files.len() > 1
        {
            out.push(ChatItem::Changes(chat::Changes {
                files: files.len(),
                added,
                removed,
                first,
            }));
            out_approvals.push(None);
        }
    };
    for (item, approval) in items.into_iter().zip(approvals) {
        if matches!(item, ChatItem::User { .. }) {
            close(&mut turn, &mut out, &mut out_approvals);
        }
        if let ChatItem::Diff(diff) = &item {
            let count = |kind| diff.lines.iter().filter(|l| l.kind == kind).count();
            let (add, remove) = (count(chat::DiffKind::Add), count(chat::DiffKind::Remove));
            let entry = turn.get_or_insert_with(|| (out.len(), Vec::new(), 0, 0));
            if !entry.1.contains(&diff.path) {
                entry.1.push(diff.path.clone());
            }
            entry.2 += add;
            entry.3 += remove;
        }
        out.push(item);
        out_approvals.push(approval);
    }
    // the turn still running isn't summed up until it's done
    if !busy {
        close(&mut turn, &mut out, &mut out_approvals);
    }
    (out, out_approvals)
}

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "Read" | "NotebookRead" => ToolKind::Read,
        "Grep" | "Glob" | "WebSearch" | "WebFetch" | "LS" => ToolKind::Search,
        "Edit" | "MultiEdit" | "Write" => ToolKind::Edit,
        _ => ToolKind::Run,
    }
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
    fn new_agents_start_with_the_defaults_from_settings() {
        let mut sessions = Sessions {
            agent_defaults: mux_core::protocol::AgentSettings {
                provider: Provider::Claude,
                model: Some("opus".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        sessions.create("New agent", SessionKind::Agent);
        sessions.create("Shell", SessionKind::Shell);
        sessions.outbox.clear();
        sessions.apply(ServerMsg::Created { id: 7 });
        sessions.apply(ServerMsg::Created { id: 8 });
        // only the agent is configured, and a shell confirmed after it doesn't take its settings
        assert_eq!(sessions.outbox.len(), 1);
        assert!(matches!(
            &sessions.outbox[0],
            ClientMsg::AgentConfigure { id: 7, settings }
                if settings.provider == Provider::Claude
                    && settings.model.as_deref() == Some("opus")
                    && settings.access == mux_core::Access::Auto
        ));
    }

    #[test]
    fn finished_turns_that_touch_several_files_get_a_summary() {
        let user = || AgentItem::User {
            text: "go".into(),
            attachments: Vec::new(),
            queued: false,
            rewind: None,
        };
        let edit = |path: &str| AgentItem::FileChange {
            changes: vec![wire::FileChange {
                path: path.into(),
                diff: "@@ -1 +1 @@\n-a\n+b\n+c".into(),
            }],
            status: ItemStatus::Done,
        };
        let transcript = vec![
            user(),
            edit("a.rs"),
            edit("b.rs"),
            edit("a.rs"),
            user(),
            edit("c.rs"),
        ];
        let (items, approvals) = chat_items(&transcript, false, Provider::Codex);
        assert_eq!(items.len(), approvals.len());
        let summaries: Vec<(usize, usize, usize, usize)> = items
            .iter()
            .filter_map(|i| match i {
                ChatItem::Changes(c) => Some((c.files, c.added, c.removed, c.first)),
                _ => None,
            })
            .collect();
        // the second turn only touched one file, so its edit card says it all
        assert_eq!(summaries, [(2, 6, 3, 1)]);
        let (busy, _) = chat_items(&transcript[..4], true, Provider::Codex);
        assert!(!busy.iter().any(|i| matches!(i, ChatItem::Changes(_))));
    }

    #[test]
    fn finished_agents_alert_once_and_stay_unread_until_looked_at() {
        let state = |status| ServerMsg::State {
            groups: vec![GroupInfo {
                id: 1,
                name: "p".into(),
                path: None,
                branch: None,
                open: true,
                sessions: vec![SessionInfo {
                    id: 2,
                    title: "fix tests".into(),
                    kind: SessionKind::Agent,
                    status,
                    meta: String::new(),
                    updated: 0,
                    provider: Provider::Claude,
                    worktree: None,
                    branch: None,
                    summary: None,
                }],
            }],
        };
        let mut sessions = Sessions::default();
        // what's already done at startup isn't news
        sessions.apply(state(SessionStatus::Done));
        assert!(sessions.take_alerts(None).is_empty());
        sessions.apply(state(SessionStatus::Running));
        sessions.apply(state(SessionStatus::Waiting));
        let alerts = sessions.take_alerts(None);
        assert!(matches!(
            alerts.as_slice(),
            [Alert {
                id: 2,
                event: Event::Waiting,
                ..
            }]
        ));
        assert!(sessions.unread.contains(&2));
        sessions.take_alerts(Some(2));
        assert!(!sessions.unread.contains(&2));
    }

    #[test]
    fn a_dragged_group_lands_in_the_gap_under_the_pointer() {
        let blocks = [(0.0, 40.0), (46.0, 200.0), (206.0, 240.0)];
        // dragging the first group past the middle of the second puts it after the second
        assert_eq!(group_gap(&blocks, 130.0, Some(0)), Some(2));
        assert_eq!(group_gap(&blocks, 300.0, Some(0)), Some(3));
        // its own gaps above and below leave it where it is
        assert_eq!(group_gap(&blocks, 10.0, Some(0)), None);
        assert_eq!(group_gap(&blocks, 100.0, Some(0)), None);
        assert_eq!(group_gap(&blocks, 10.0, Some(2)), Some(0));
    }

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
            AgentItem::User {
                text: "hi".into(),
                attachments: Vec::new(),
                queued: false,
                rewind: None,
            },
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
        let (items, approvals) = chat_items(&transcript, true, Provider::Codex);
        // no thinking indicator while the approval waits on the user
        assert_eq!(items.len(), 4, "user, two diffs, approval");
        assert_eq!(approvals, vec![None, None, None, Some(9)]);
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
