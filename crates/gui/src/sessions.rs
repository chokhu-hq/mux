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
use mux_ui::{GroupHeader, MenuItem, SessionRow, TooltipExt, icons};

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
    // commands the agent left running in the background, and ones that just finished
    pub background: Vec<wire::Background>,
    // files picked or dropped for the next message
    pub attachments: Vec<String>,
    transcript: Vec<AgentItem>,
    settled: Settled,
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
        self.settled = Settled::default();
        self.rebuild_latest();
    }

    // a change at this place in the transcript, which often leaves everything before the latest message alone
    fn changed(&mut self, index: usize) {
        if index < self.settled.transcript {
            self.rebuild();
        } else {
            self.rebuild_latest();
        }
    }

    fn rebuild_latest(&mut self) {
        let Settled {
            transcript: from,
            chat: at,
            waiting,
        } = self.settled;
        self.chat.truncate(at);
        self.approvals.truncate(at);
        let (items, approvals) = chat_items_from(
            &self.transcript,
            &self.settled,
            self.busy,
            self.settings.provider,
        );
        self.chat.extend(items);
        self.approvals.extend(approvals);
        let is_message = |item: &AgentItem| matches!(item, AgentItem::User { .. });
        if let Some(message) = self.transcript[from..].iter().rposition(is_message)
            && let Some(shown) = self.chat[at..]
                .iter()
                .rposition(|item| matches!(item, ChatItem::User { .. }))
        {
            self.settled = Settled {
                transcript: from + message,
                chat: at + shown,
                waiting: waiting || self.chat[at..at + shown].iter().any(asks_for_a_decision),
            };
        }
    }

    // adds streamed text to the reply at the end of the chat, if that's what the chat shows last
    fn append_in_place(&mut self, index: usize, piece: &str) -> bool {
        if index + 1 != self.transcript.len() {
            return false;
        }
        let Some(current) = self.transcript[index].streamed_text() else {
            return false;
        };
        let last = self
            .chat
            .iter_mut()
            .rev()
            .find(|item| !matches!(item, ChatItem::Thinking));
        let (Some(ChatItem::Assistant(shown)) | Some(ChatItem::Reasoning { text: shown, .. })) =
            last
        else {
            return false;
        };
        if shown.as_str() != &current[..current.len() - piece.len()] {
            return false;
        }
        shown.push_str(piece);
        true
    }
}

// how much of the chat was built for good: everything before the latest message, which is done with
#[derive(Default)]
struct Settled {
    transcript: usize,
    chat: usize,
    // whether any of it still waits on a decision, which keeps the agent from showing as thinking
    waiting: bool,
}

fn asks_for_a_decision(item: &ChatItem) -> bool {
    matches!(item, ChatItem::Permission(request) if request.decision.is_none())
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
    NewShell(u64),
    Split(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    Finished,
    Waiting,
    Failed,
}

// seconds an agent has to stay finished before that's announced
const SETTLE: f64 = 3.0;

pub struct Alert {
    pub id: u64,
    pub title: String,
    pub event: Event,
}

struct Creating {
    settings: Option<mux_core::protocol::AgentSettings>,
    // opened on the right instead of taking over the main view
    split: bool,
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
    // each agent account's banked limit resets, and how using one is going
    resets: HashMap<Provider, Vec<wire::LimitReset>>,
    reset_pending: Option<Provider>,
    reset_note: Option<(Provider, String)>,
    // the last month of token use and which day is today, once the server has read the clis' history
    tokens: Option<(i64, Vec<UsageDay>)>,
    // sessions where an agent finished or stopped to ask something, since they were last looked at
    unread: HashSet<u64>,
    // agents that just finished, failed or started waiting, for the app to point out
    // what to tell the person, with when a finish was first seen so it can settle before it's announced
    alerts: Vec<(Alert, Option<f64>)>,
    pub outbox: Vec<ClientMsg>,
    // what each session asked for should start with, in the order the server confirms them
    creating: std::collections::VecDeque<Creating>,
    // the terminal shown on the right beside the selected session
    pub split: Option<u64>,
    // the settings a new agent starts with, chosen in settings
    pub agent_defaults: mux_core::protocol::AgentSettings,
}

impl Sessions {
    // what agents did since the last call, marking each unread unless it's the one being looked at
    fn status(&self, id: u64) -> Option<SessionStatus> {
        self.infos().find(|s| s.id == id).map(|s| s.status)
    }

    // a finish waiting to be announced, so the window wakes when it's due
    pub fn alert_due(&self) -> bool {
        !self.alerts.is_empty()
    }

    // the alerts ready at `now`, in seconds. a finish only counts once it has lasted a moment, since some agents
    // carry straight on by themselves, like a claude its own background task wakes, or one mux picks back up
    pub fn take_alerts(&mut self, looking: Option<u64>, now: f64) -> Vec<Alert> {
        let mut alerts = Vec::new();
        for (alert, since) in std::mem::take(&mut self.alerts) {
            let since = since.unwrap_or(now);
            if alert.event == Event::Finished {
                // it started working again, or went away, so there's nothing to announce
                if self.status(alert.id) != Some(SessionStatus::Done) {
                    continue;
                }
                if now - since < SETTLE {
                    self.alerts.push((alert, Some(since)));
                    continue;
                }
            }
            alerts.push(alert);
        }
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
                provider: (session.kind == SessionKind::Agent).then_some(session.provider),
            })
            .collect()
    }

    pub fn select(&mut self, id: u64) {
        // a session can only be shown in one place, so picking the one on the right moves it to the main view
        if self.split == Some(id) {
            self.split = None;
        }
        self.selected = Some(id);
        self.picked = true;
    }

    // shows a terminal for the selected session's project on the right, or hides the one that's there
    pub fn toggle_split_terminal(&mut self) {
        if self.split.take().is_some() {
            return;
        }
        let Some((g, _)) = self.selected.and_then(|id| self.locate(id)) else {
            return;
        };
        let group = &self.groups[g];
        let shells: Vec<&SessionInfo> = group
            .sessions
            .iter()
            .filter(|s| s.kind == SessionKind::Shell && Some(s.id) != self.selected)
            .collect();
        // a running shell is reused first, then one that exited is started again, and only then is one made
        let live = shells.iter().find(|s| !ended(s.status)).map(|s| s.id);
        let exited = shells.first().map(|s| s.id);
        match (live, exited) {
            (Some(id), _) => self.split = Some(id),
            (None, Some(id)) => {
                self.restart(id);
                self.split = Some(id);
            }
            (None, None) => {
                let group = group.id;
                self.request(Some(group), "Shell", SessionKind::Shell, true);
            }
        }
    }

    // runs a terminal's command again, dropping what the old run printed so it's attached afresh
    pub fn restart(&mut self, id: u64) {
        self.outbox.push(ClientMsg::Restart { id });
        if let Some(local) = self.locals.get_mut(&id) {
            local.terminal = None;
            local.attaching = false;
        }
    }

    // the session for the pane on the right, which may still be fading out after it was closed
    pub fn split_session(&mut self, id: u64) -> Option<SplitSession<'_>> {
        let found = self.groups.iter().find_map(|g| {
            let info = g.sessions.iter().find(|s| s.id == id)?;
            Some((info, g.path.clone()))
        });
        let Some((info, folder)) = found else {
            // removed elsewhere, so there's nothing left to show
            if self.split == Some(id) {
                self.split = None;
            }
            return None;
        };
        let local = self.locals.entry(id).or_default();
        Some((info, local, &mut self.outbox, &self.models, folder))
    }

    pub fn selected_id(&self) -> Option<u64> {
        self.selected
    }

    pub fn take_picked(&mut self) -> bool {
        std::mem::take(&mut self.picked)
    }

    pub fn apply(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Welcome { .. }
            | ServerMsg::Build { .. }
            | ServerMsg::Focus
            | ServerMsg::Quit => {}
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
                    self.alerts.extend(alerts.into_iter().map(|a| (a, None)));
                    // a shell left with exit closes like a terminal tab, while one that failed stays to show why
                    let exited: Vec<u64> = self
                        .infos()
                        .filter(|s| s.kind == SessionKind::Shell && s.status == SessionStatus::Done)
                        .filter(|s| before.get(&s.id) == Some(&SessionStatus::Running))
                        .map(|s| s.id)
                        .collect();
                    for id in exited {
                        self.remove(id);
                    }
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
                let creating = self.creating.pop_front();
                // a new agent takes the defaults from settings before its first message
                if let Some(settings) = creating.as_ref().and_then(|c| c.settings.clone()) {
                    self.outbox.push(ClientMsg::AgentConfigure { id, settings });
                }
                if creating.is_some_and(|c| c.split) {
                    self.split = Some(id);
                    return;
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
            ServerMsg::AgentBackground { id, tasks } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.background = tasks;
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
            ServerMsg::AgentTranscriptMore { id, items } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.transcript.extend(items);
                    local.rebuild_latest();
                }
            }
            ServerMsg::AgentItem { id, index, item } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    match local.transcript.get_mut(index) {
                        Some(slot) => *slot = item,
                        None => local.transcript.push(item),
                    }
                    local.changed(index);
                }
            }
            ServerMsg::AgentAppend {
                id,
                index,
                at,
                text,
            } => {
                if let Some(local) = self.locals.get_mut(&id)
                    && let Some(current) = local
                        .transcript
                        .get_mut(index)
                        .and_then(AgentItem::streamed_text)
                {
                    // text already there came with a full copy of the item sent after this was queued
                    if current.len() == at {
                        current.push_str(&text);
                        if !local.append_in_place(index, &text) {
                            local.changed(index);
                        }
                    } else if current.len() < at {
                        log::warn!(
                            "missed streamed text in session {id}, it'll catch up when the item finishes"
                        );
                    }
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
            ServerMsg::AgentResets { provider, resets } => {
                self.resets.insert(provider, resets);
            }
            ServerMsg::ResetResult { provider, result } => {
                if self.reset_pending == Some(provider) {
                    self.reset_pending = None;
                }
                let note = match result {
                    wire::ResetResult::Reset => None,
                    wire::ResetResult::NothingToReset => Some(
                        "There was no limit to lift right now, so your reset was kept.".to_owned(),
                    ),
                    wire::ResetResult::NoneLeft => {
                        Some("There are no resets left on this account.".to_owned())
                    }
                    wire::ResetResult::Failed(why) => Some(format!("Couldn't use a reset: {why}")),
                };
                self.reset_note = note.map(|n| (provider, n));
            }
            ServerMsg::TokenHistory { today, days } => self.tokens = Some((today, days)),
            ServerMsg::AgentModels {
                provider,
                mut models,
            } => {
                for model in &mut models {
                    name_default(model);
                }
                self.models.insert(provider, models);
            }
            ServerMsg::AgentBusy { id, busy } => {
                if let Some(local) = self.locals.get_mut(&id) {
                    local.busy = busy;
                    local.rebuild_latest();
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

    // whether restarting the server would cut something off: a terminal or agent at work, or a command an agent left
    // running, and until the server's sessions are known that's assumed
    pub fn is_busy(&self) -> bool {
        if !self.loaded {
            return true;
        }
        let working = self
            .groups
            .iter()
            .flat_map(|g| &g.sessions)
            .any(|s| matches!(s.status, SessionStatus::Running | SessionStatus::Waiting));
        let background = self.locals.values().any(|local| {
            local
                .background
                .iter()
                .any(|task| task.state == wire::BackgroundState::Running)
        });
        working || background
    }

    pub fn tokens(&self) -> Option<&(i64, Vec<UsageDay>)> {
        self.tokens.as_ref()
    }

    pub fn models(&self) -> &Models {
        &self.models
    }

    // what the limit card of an agent on `provider` can offer, for codex only since only it banks resets
    pub fn reset_offer(&self, provider: Provider) -> Option<mux_ui::chat::ResetOffer> {
        (provider == Provider::Codex).then(|| mux_ui::chat::ResetOffer {
            left: self.resets.get(&provider).map_or(0, Vec::len),
            pending: self.reset_pending == Some(provider),
            note: self
                .reset_note
                .as_ref()
                .filter(|(p, _)| *p == provider)
                .map(|(_, note)| note.clone()),
        })
    }

    pub fn use_reset(&mut self, provider: Provider) {
        self.reset_pending = Some(provider);
        self.reset_note = None;
        self.outbox.push(ClientMsg::UseReset { provider });
    }

    pub fn resets_left(&self, provider: Provider) -> usize {
        self.resets.get(&provider).map_or(0, Vec::len)
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
        self.request(group, title, kind, false);
    }

    // every new session goes through here, so the server's confirmations line up with what was asked for
    fn request(&mut self, group: Option<u64>, title: &str, kind: SessionKind, split: bool) {
        let settings = (kind == SessionKind::Agent).then(|| self.agent_defaults.clone());
        self.creating.push_back(Creating { settings, split });
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
        if self.split == Some(id) {
            self.split = None;
        }
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
                let shown = ui
                    .push_id(group.id, |ui| {
                        GroupHeader::new(&group.name)
                            .open(group.open)
                            .rename(draft)
                            .show(ui)
                    })
                    .inner;
                if let Some(input) = shown.field {
                    if !input.has_focus() && !input.lost_focus() {
                        input.request_focus();
                        select_all(ui.ctx(), input.id, draft);
                    }
                    let (enter, escape) =
                        ui.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                    if input.lost_focus() {
                        if enter && !escape {
                            actions.push(Action::Rename(group.id, std::mem::take(draft)));
                        }
                        actions.push(Action::EndRename);
                    }
                }
            } else {
                let shown = ui
                    .push_id(group.id, |ui| {
                        GroupHeader::new(&group.name)
                            // a closed project still says when something in it needs looking at
                            .status(
                                (!group.open)
                                    .then(|| most_pressing(&group.sessions, &self.unread))
                                    .flatten(),
                            )
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
                    if MenuItem::new("New agent")
                        .icon(icons::ROBOT)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::NewAgent(group.id));
                        ui.close();
                    }
                    if MenuItem::new("New shell")
                        .icon(icons::TERMINAL_WINDOW)
                        .show(ui)
                        .clicked()
                    {
                        actions.push(Action::NewShell(group.id));
                        ui.close();
                    }
                    folder_items(ui, group.path.as_deref());
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
                        && MenuItem::new("Remove project")
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
                    // a row being renamed keeps everything but its title, which becomes the field
                    let renaming = self
                        .renaming
                        .as_mut()
                        .filter(|(id, _)| *id == session.id)
                        .map(|(_, draft)| draft);
                    let (row, field) = rows
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
                            if let Some(draft) = renaming {
                                row = row.rename(draft);
                            }
                            row.show_with_field(ui)
                        })
                        .inner;
                    if let Some(input) = field {
                        if !input.has_focus() && !input.lost_focus() {
                            input.request_focus();
                            if let Some((_, draft)) = &self.renaming {
                                select_all(rows.ctx(), input.id, draft);
                            }
                        }
                        let (enter, escape) =
                            rows.input(|i| (i.key_pressed(Key::Enter), i.key_pressed(Key::Escape)));
                        if input.lost_focus() {
                            if enter
                                && !escape
                                && let Some((_, draft)) = &mut self.renaming
                            {
                                actions.push(Action::Rename(session.id, std::mem::take(draft)));
                            }
                            actions.push(Action::EndRename);
                        }
                        continue;
                    }
                    if row.clicked() {
                        actions.push(Action::Select(session.id));
                    }
                    if row.double_clicked() {
                        actions.push(Action::StartRename(session.id, session.title.clone()));
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
                        folder_items(ui, session.worktree.as_deref().or(group.path.as_deref()));
                        if let Some(branch) = session.branch.as_ref().or(group.branch.as_ref()) {
                            mux_ui::copy_item(ui, "Copy branch name", branch);
                        }
                        if MenuItem::new("Rename")
                            .icon(icons::PENCIL_SIMPLE)
                            .show(ui)
                            .clicked()
                        {
                            actions.push(Action::StartRename(session.id, session.title.clone()));
                            ui.close();
                        }
                        if self.selected != Some(session.id)
                            && MenuItem::new("Open on the right")
                                .icon(icons::SIDEBAR_SIMPLE)
                                .show(ui)
                                .clicked()
                        {
                            actions.push(Action::Split(session.id));
                            ui.close();
                        }
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
                    self.request(Some(group), "New agent", SessionKind::Agent, false)
                }
                Action::Split(id) => self.split = Some(id),
                Action::NewShell(group) => {
                    self.request(Some(group), "Shell", SessionKind::Shell, false)
                }
                Action::StartRename(id, name) => self.renaming = Some((id, name)),
                // groups and sessions share one run of ids, so the id says which is being renamed
                Action::Rename(id, name) => {
                    let group = self.groups.iter().any(|g| g.id == id);
                    self.outbox.push(if group {
                        ClientMsg::RenameGroup { id, name }
                    } else {
                        ClientMsg::RenameSession { id, title: name }
                    });
                }
                Action::EndRename => self.renaming = None,
                Action::ChangeFolder(id) => self.set_group_folder(id),
                Action::RemoveGroup(id) => self.outbox.push(ClientMsg::DeleteGroup { id }),
                Action::Select(id) => self.select(id),
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

// the folder a project or session works in, to copy or open in the file manager
fn folder_items(ui: &mut egui::Ui, folder: Option<&str>) {
    let Some(folder) = folder else {
        return;
    };
    mux_ui::copy_item(ui, "Copy folder path", folder);
    if MenuItem::new("Open folder")
        .icon(icons::FOLDER_OPEN)
        .show(ui)
        .clicked()
    {
        mux_ui::chat::open_externally(&mux_core::paths::local(folder));
        ui.close();
    }
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
        // an agent with a conversation that's simply not working right now has started, unlike a new one
        (SessionStatus::Idle, SessionKind::Agent)
            if session.summary.as_ref().is_some_and(|s| s.messages > 0) =>
        {
            "Idle".to_owned()
        }
        (SessionStatus::Idle, _) => "Not started".to_owned(),
        (SessionStatus::Running, SessionKind::Agent) => "Working…".to_owned(),
        (SessionStatus::Done, SessionKind::Agent) => "Finished".to_owned(),
        (SessionStatus::Failed, _) if session.meta.is_empty() => "Failed".to_owned(),
        _ => session.meta.clone(),
    }
}

// the model an agent runs as people know it, falling back to its id and then to the cli's default
// claude's default entry says which model it is only in its description, "Opus 5.5 · Best for everyday tasks",
// so its name carries the model wherever it shows, even from a server that hasn't named it that way
fn name_default(model: &mut ModelOption) {
    if model.id == "default"
        && let Some((name, _)) = model.description.split_once(" · ")
        && !model.name.contains(name)
    {
        model.name = format!("Default ({name})");
    }
}

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

#[cfg(test)]
fn chat_items(
    transcript: &[AgentItem],
    busy: bool,
    provider: Provider,
) -> (Vec<ChatItem>, Vec<Option<u64>>) {
    chat_items_from(transcript, &Settled::default(), busy, provider)
}

// the chat items for what comes after the settled part of the transcript
fn chat_items_from(
    transcript: &[AgentItem],
    settled: &Settled,
    busy: bool,
    provider: Provider,
) -> (Vec<ChatItem>, Vec<Option<u64>>) {
    let mut items = Vec::new();
    let mut approvals = Vec::new();
    let legacy_retry = |item: &AgentItem| {
        matches!(item,
            AgentItem::Error { message } if message.starts_with("Mux will carry on by itself ")
        )
    };
    for (position, entry) in transcript.iter().enumerate().skip(settled.transcript) {
        let mut approval = None;
        // older servers saved one limit event as a reply followed by two errors
        if let AgentItem::Assistant { text } = entry
            && let Some(AgentItem::Error { message }) = transcript.get(position + 1)
            && text.trim() == message.trim()
            && transcript.get(position + 2).is_some_and(legacy_retry)
        {
            continue;
        }
        match entry {
            AgentItem::User {
                text,
                attachments,
                queued,
                took,
                ..
            } => items.push(ChatItem::User {
                took: *took,
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
                background,
                ..
            } => items.push(ChatItem::Tool(ToolCall {
                kind: ToolKind::Run,
                target: shell_body(command),
                status: tool_status(*status),
                output: (!output.is_empty()).then(|| output.clone()),
                background: *background,
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
                        background: false,
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
                background: false,
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
            AgentItem::Reasoning {
                text,
                started,
                ended,
            } => items.push(ChatItem::Reasoning {
                text: text.clone(),
                started: *started,
                ended: *ended,
            }),
            AgentItem::Error { message } => {
                if let Some(AgentItem::Error { message: retry }) = transcript.get(position + 1)
                    && transcript.get(position + 1).is_some_and(legacy_retry)
                {
                    items.push(ChatItem::UsageLimit {
                        message: format!("{message}\n\n{retry}"),
                        retry_at: None,
                    });
                } else if legacy_retry(entry)
                    && position > 0
                    && matches!(transcript[position - 1], AgentItem::Error { .. })
                {
                    continue;
                } else {
                    items.push(ChatItem::Error(message.clone()))
                }
            }
            AgentItem::UsageLimit { message, retry_at } => {
                let replied = transcript[position + 1..]
                    .iter()
                    .any(|item| matches!(item, AgentItem::User { .. }));
                items.push(ChatItem::UsageLimit {
                    message: message.clone(),
                    retry_at: if replied { None } else { *retry_at },
                });
            }
            AgentItem::Retry {
                reason,
                attempt,
                of,
                status,
            } => items.push(ChatItem::Retry {
                reason: reason.clone(),
                attempt: *attempt,
                of: *of,
                status: match status {
                    ItemStatus::Running => ToolStatus::Running,
                    ItemStatus::Done => ToolStatus::Done,
                    ItemStatus::Failed | ItemStatus::Declined => ToolStatus::Failed,
                },
            }),
        }
        approvals.push(approval);
    }
    approvals.resize(items.len(), None);
    let (mut items, approvals) = summarise_changes(items, approvals, busy, settled.chat);
    chat::set_thinking(&mut items, busy && !settled.waiting);
    (items, approvals)
}

// a finished turn that touched several files ends with one line that opens them all for review
fn summarise_changes(
    items: Vec<ChatItem>,
    approvals: Vec<Option<u64>>,
    busy: bool,
    // where these items start in the chat, since a summary points at its first edit card
    offset: usize,
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
            let entry = turn.get_or_insert_with(|| (offset + out.len(), Vec::new(), 0, 0));
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

// a name being changed starts out selected, so typing replaces it the way renaming a file does,
// with the cursor at the start so a long name shows from its beginning rather than its end
fn select_all(ctx: &egui::Context, id: Id, text: &str) {
    let mut state = egui::TextEdit::load_state(ctx, id).unwrap_or_default();
    let end = egui::text::CCursor::new(text.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::two(
            end,
            egui::text::CCursor::new(0),
        )));
    state.store(ctx, id);
}

// a session on the right: its info and local state, the outbox, the model lists and its project folder
pub type SplitSession<'a> = (
    &'a SessionInfo,
    &'a mut Local,
    &'a mut Vec<ClientMsg>,
    &'a Models,
    Option<String>,
);

pub fn ended(status: SessionStatus) -> bool {
    matches!(status, SessionStatus::Done | SessionStatus::Failed)
}

// the status most worth knowing about among a project's sessions: one waiting on the person, then one that
// failed or finished since it was last looked at, with work in progress in between
fn most_pressing(sessions: &[SessionInfo], unread: &HashSet<u64>) -> Option<SessionStatus> {
    sessions
        .iter()
        .filter_map(|session| {
            let unseen = unread.contains(&session.id);
            let rank = match session.status {
                SessionStatus::Waiting => 4,
                SessionStatus::Failed if unseen => 3,
                // a shell runs for as long as it's open, which is nothing to point out
                SessionStatus::Running if session.kind != SessionKind::Shell => 2,
                SessionStatus::Done if unseen => 1,
                _ => return None,
            };
            Some((rank, session.status))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, status)| status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_limit_messages_become_one_notice_and_real_errors_stay_errors() {
        let transcript = vec![
            AgentItem::Assistant {
                text: "Weekly limit reached".into(),
            },
            AgentItem::Error {
                message: "Weekly limit reached".into(),
            },
            AgentItem::Error {
                message: "Mux will carry on by itself in about 34 h, once the limit resets.".into(),
            },
            AgentItem::Error {
                message: "Connection failed".into(),
            },
        ];
        let (items, approvals) = chat_items(&transcript, false, Provider::Claude);
        assert_eq!(items.len(), 2);
        assert_eq!(approvals.len(), 2);
        assert!(matches!(&items[0], ChatItem::UsageLimit { message, .. }
            if message.matches("Weekly limit reached").count() == 1 && message.contains("34 h")));
        assert!(matches!(&items[1], ChatItem::Error(text) if text == "Connection failed"));
    }

    #[test]
    fn sending_another_message_hides_the_old_automatic_retry_status() {
        let transcript = vec![
            AgentItem::UsageLimit {
                message: "Limit reached".into(),
                retry_at: Some(100),
            },
            AgentItem::User {
                text: "try again".into(),
                attachments: Vec::new(),
                queued: false,
                rewind: None,
                took: None,
            },
        ];
        let (items, _) = chat_items(&transcript, false, Provider::Claude);
        assert!(matches!(
            &items[0],
            ChatItem::UsageLimit { retry_at: None, .. }
        ));
    }

    #[test]
    fn the_default_model_says_which_model_it_is() {
        let mut default = ModelOption {
            id: "default".into(),
            name: "Default (recommended)".into(),
            description: "Opus 5.5 · Best for everyday, complex tasks".into(),
            efforts: Vec::new(),
            default_effort: String::new(),
            is_default: true,
            fast: None,
        };
        name_default(&mut default);
        assert_eq!(default.name, "Default (Opus 5.5)");
        name_default(&mut default);
        assert_eq!(
            default.name, "Default (Opus 5.5)",
            "naming it twice changes nothing"
        );
    }

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
            took: None,
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
    fn a_finish_is_announced_only_once_it_lasts() {
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
        sessions.apply(state(SessionStatus::Running));
        sessions.apply(state(SessionStatus::Done));
        assert!(
            sessions.take_alerts(None, 10.0).is_empty(),
            "not straight away"
        );
        assert!(sessions.take_alerts(None, 11.0).is_empty());
        assert!(!sessions.unread.contains(&2), "and no dot yet either");
        // it carried on by itself before the moment was up
        sessions.apply(state(SessionStatus::Running));
        assert!(sessions.take_alerts(None, 20.0).is_empty());
        assert!(!sessions.alert_due());

        sessions.apply(state(SessionStatus::Done));
        assert!(sessions.take_alerts(None, 30.0).is_empty());
        let alerts = sessions.take_alerts(None, 33.5);
        assert!(matches!(
            alerts.as_slice(),
            [Alert {
                id: 2,
                event: Event::Finished,
                ..
            }]
        ));
        assert!(sessions.unread.contains(&2));
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
        assert!(sessions.take_alerts(None, 0.0).is_empty());
        sessions.apply(state(SessionStatus::Running));
        sessions.apply(state(SessionStatus::Waiting));
        let alerts = sessions.take_alerts(None, 0.0);
        assert!(matches!(
            alerts.as_slice(),
            [Alert {
                id: 2,
                event: Event::Waiting,
                ..
            }]
        ));
        assert!(sessions.unread.contains(&2));
        sessions.take_alerts(Some(2), 0.0);
        assert!(!sessions.unread.contains(&2));
    }

    #[test]
    fn streamed_text_joins_the_reply_once_and_in_place() {
        let mut sessions = Sessions::default();
        sessions.locals.insert(5, Local::default());
        sessions.apply(ServerMsg::AgentTranscript {
            id: 5,
            items: vec![AgentItem::Assistant { text: "Hel".into() }],
            busy: true,
            settings: AgentSettings::default(),
            context: None,
        });
        let append = |at: usize, text: &str| ServerMsg::AgentAppend {
            id: 5,
            index: 0,
            at,
            text: text.into(),
        };
        sessions.apply(append(3, "lo"));
        // the same piece again, from before a full copy of the reply was sent
        sessions.apply(append(3, "lo"));
        sessions.apply(append(99, "?"));
        assert_eq!(
            sessions.locals[&5].transcript,
            [AgentItem::Assistant {
                text: "Hello".into()
            }]
        );
    }

    #[test]
    fn a_closed_project_shows_what_most_needs_looking_at() {
        let session = |id, kind, status| SessionInfo {
            id,
            title: String::new(),
            kind,
            status,
            meta: String::new(),
            updated: 0,
            provider: Provider::Claude,
            worktree: None,
            branch: None,
            summary: None,
        };
        let shell = session(1, SessionKind::Shell, SessionStatus::Running);
        let done = session(2, SessionKind::Agent, SessionStatus::Done);
        let failed = session(3, SessionKind::Agent, SessionStatus::Failed);
        let working = session(4, SessionKind::Agent, SessionStatus::Running);
        let waiting = session(5, SessionKind::Agent, SessionStatus::Waiting);
        let mut unread = HashSet::new();
        assert_eq!(
            most_pressing(std::slice::from_ref(&shell), &unread),
            None,
            "an open shell is nothing new"
        );
        assert_eq!(
            most_pressing(&[done.clone(), failed.clone()], &unread),
            None,
            "already seen"
        );
        unread.extend([2, 3]);
        assert_eq!(
            most_pressing(&[done.clone(), working.clone()], &unread),
            Some(SessionStatus::Running)
        );
        assert_eq!(
            most_pressing(&[done.clone(), working.clone(), failed.clone()], &unread),
            Some(SessionStatus::Failed)
        );
        assert_eq!(
            most_pressing(&[shell, done, failed, working, waiting], &unread),
            Some(SessionStatus::Waiting)
        );
    }

    #[test]
    fn a_reset_is_offered_for_codex_and_says_what_happened() {
        let mut sessions = Sessions::default();
        sessions.apply(ServerMsg::AgentResets {
            provider: Provider::Codex,
            resets: vec![
                wire::LimitReset {
                    title: "Full reset".into(),
                    expires_at: None,
                };
                2
            ],
        });
        assert!(
            sessions.reset_offer(Provider::Claude).is_none(),
            "claude has no resets"
        );
        assert_eq!(sessions.reset_offer(Provider::Codex).unwrap().left, 2);
        sessions.use_reset(Provider::Codex);
        assert!(sessions.reset_offer(Provider::Codex).unwrap().pending);
        assert!(matches!(
            sessions.outbox.last(),
            Some(ClientMsg::UseReset {
                provider: Provider::Codex
            })
        ));
        sessions.apply(ServerMsg::ResetResult {
            provider: Provider::Codex,
            result: wire::ResetResult::NothingToReset,
        });
        let offer = sessions.reset_offer(Provider::Codex).unwrap();
        assert!(!offer.pending);
        assert!(offer.note.unwrap().contains("kept"));
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
                took: None,
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

    // a long chat: 200 turns of ten capped tool outputs, an edit and a reply, about 50 MB
    fn big_chat(turns: usize) -> Vec<AgentItem> {
        let mut items = Vec::new();
        for turn in 0..turns {
            items.push(AgentItem::User {
                text: format!("message {turn}"),
                attachments: Vec::new(),
                queued: false,
                rewind: None,
                took: None,
            });
            for _ in 0..10 {
                items.push(AgentItem::Command {
                    command: "cargo test --workspace".into(),
                    output: "test result: ok\n".repeat(1600),
                    exit_code: Some(0),
                    status: ItemStatus::Done,
                    background: false,
                });
            }
            items.push(AgentItem::FileChange {
                changes: vec![wire::FileChange {
                    path: "/tmp/src/lib.rs".into(),
                    diff: "@@ -1 +1 @@\n".to_owned() + &"+let a = 1;\n".repeat(200),
                }],
                status: ItemStatus::Done,
            });
            items.push(AgentItem::Assistant {
                text: "Fixed it. Here's **what** changed:\n\n- one\n- two\n\n".repeat(20),
            });
        }
        items
    }

    // run with: cargo test -p mux-gui --release bench_ -- --ignored --nocapture
    #[test]
    #[ignore]
    fn bench_updates_in_a_long_chat() {
        let id = 5;
        let mut sessions = Sessions::default();
        sessions.locals.insert(id, Local::default());
        let items = big_chat(200);
        let size: usize = items
            .iter()
            .map(|item| mux_core::protocol::encode(item).unwrap().len())
            .sum();
        let start = std::time::Instant::now();
        sessions.apply(ServerMsg::AgentTranscript {
            id,
            items,
            busy: true,
            settings: AgentSettings::default(),
            context: None,
        });
        println!("opening {} MB: {:?}", size / (1024 * 1024), start.elapsed());
        let start = std::time::Instant::now();
        let last = sessions.locals[&id].transcript.len() - 1;
        let mut at = match &sessions.locals[&id].transcript[last] {
            AgentItem::Assistant { text } => text.len(),
            _ => unreachable!(),
        };
        for _ in 0..100 {
            sessions.apply(ServerMsg::AgentAppend {
                id,
                index: last,
                at,
                text: "word ".into(),
            });
            at += 5;
        }
        println!("a piece of streamed text: {:?}", start.elapsed() / 100);
        let start = std::time::Instant::now();
        for _ in 0..20 {
            sessions.apply(ServerMsg::AgentItem {
                id,
                index: last + 1,
                item: AgentItem::Command {
                    command: "ls".into(),
                    output: String::new(),
                    exit_code: None,
                    status: ItemStatus::Running,
                    background: false,
                },
            });
        }
        println!("a new tool call: {:?}", start.elapsed() / 20);

        // one long turn, where there's no earlier message to stop at
        let mut items = big_chat(200);
        items.retain(|item| !matches!(item, AgentItem::User { .. }));
        items.insert(0, big_chat(1).remove(0));
        items.push(AgentItem::Assistant { text: "Hi".into() });
        let last = items.len() - 1;
        sessions.apply(ServerMsg::AgentTranscript {
            id,
            items,
            busy: true,
            settings: AgentSettings::default(),
            context: None,
        });
        let start = std::time::Instant::now();
        for i in 0..100 {
            sessions.apply(ServerMsg::AgentAppend {
                id,
                index: last,
                at: 2 + i * 5,
                text: "word ".into(),
            });
        }
        println!(
            "streamed text in one long turn: {:?}",
            start.elapsed() / 100
        );
        let start = std::time::Instant::now();
        for _ in 0..20 {
            sessions.apply(ServerMsg::AgentItem {
                id,
                index: last + 1,
                item: AgentItem::Command {
                    command: "ls".into(),
                    output: String::new(),
                    exit_code: None,
                    status: ItemStatus::Running,
                    background: false,
                },
            });
        }
        println!(
            "a new tool call in one long turn: {:?}",
            start.elapsed() / 20
        );
    }

    // redoing only the latest turn has to end up exactly where building the whole chat does
    #[test]
    fn updating_the_latest_turn_matches_building_the_whole_chat() {
        let user = |queued| AgentItem::User {
            text: "go".into(),
            attachments: Vec::new(),
            queued,
            rewind: None,
            took: None,
        };
        let edit = |path: &str| AgentItem::FileChange {
            changes: vec![wire::FileChange {
                path: path.into(),
                diff: "@@ -1 +1 @@\n-a\n+b\n".into(),
            }],
            status: ItemStatus::Done,
        };
        let kinds = |pick: u64| match pick % 11 {
            0 | 1 => user(pick % 22 == 1),
            2 => AgentItem::Assistant {
                text: String::new(),
            },
            3 => AgentItem::Assistant {
                text: "done".into(),
            },
            4 => AgentItem::Reasoning {
                text: "hm".into(),
                started: 1,
                ended: None,
            },
            5 => edit("a"),
            6 => edit("b"),
            7 => AgentItem::Plan {
                text: "plan".into(),
            },
            8 => AgentItem::UsageLimit {
                message: "limit".into(),
                retry_at: Some(9),
            },
            9 => AgentItem::Approval {
                id: pick,
                title: "ok?".into(),
                detail: String::new(),
                decision: pick.is_multiple_of(2).then_some(wire::Decision::Allow),
            },
            _ => AgentItem::Error {
                message: "oops".into(),
            },
        };
        let mut sessions = Sessions::default();
        sessions.locals.insert(1, Local::default());
        let mut seed = 7u64;
        let mut roll = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            seed >> 33
        };
        for _ in 0..3000 {
            let (pick, len) = (roll(), sessions.locals[&1].transcript.len());
            let msg = match roll() % 10 {
                // a change to something already there, sometimes well before the latest message
                0 | 1 if len > 0 => ServerMsg::AgentItem {
                    id: 1,
                    index: roll() as usize % len,
                    item: kinds(pick),
                },
                2 => ServerMsg::AgentBusy {
                    id: 1,
                    busy: pick.is_multiple_of(2),
                },
                3 | 4 if len > 0 => {
                    let index = if pick.is_multiple_of(4) {
                        roll() as usize % len
                    } else {
                        len - 1
                    };
                    let mut item = sessions.locals[&1].transcript[index].clone();
                    match item.streamed_text() {
                        Some(text) => ServerMsg::AgentAppend {
                            id: 1,
                            index,
                            at: text.len(),
                            text: if pick.is_multiple_of(3) {
                                " ".into()
                            } else {
                                "word".into()
                            },
                        },
                        None => ServerMsg::AgentBusy { id: 1, busy: true },
                    }
                }
                _ => ServerMsg::AgentItem {
                    id: 1,
                    index: len,
                    item: kinds(pick),
                },
            };
            sessions.apply(msg);
            let local = &sessions.locals[&1];
            let (whole, approvals) =
                chat_items(&local.transcript, local.busy, local.settings.provider);
            assert_eq!(format!("{:?}", local.chat), format!("{whole:?}"));
            assert_eq!(local.approvals, approvals);
        }
    }
}
