mod asks;
mod attachments;
mod background;
mod cards;
mod composer;
mod markdown;
mod queue;
mod slash;

use egui::{Rect, ScrollArea, Sense, Ui, UiBuilder, pos2};
use mux_core::protocol::ContextUsage;

pub use attachments::open_externally;
pub use cards::thinking;
pub use composer::{Composer, ComposerAction, Controls};
pub use markdown::markdown;
pub use slash::SlashCommand;

use crate::components::EmptyState;
use crate::icons;
use crate::theme::{self, space};

const MAX_WIDTH: f32 = 720.0;

#[derive(Debug)]
pub enum ChatItem {
    User {
        text: String,
        attachments: Vec<String>,
        // sent while the agent was busy and not picked up yet
        queued: bool,
        // how long the turn it started took, in milliseconds, once it's over
        took: Option<u64>,
    },
    Assistant(String),
    // something went wrong, set apart from the conversation so it isn't read as the agent talking
    Error(String),
    UsageLimit {
        message: String,
        retry_at: Option<u64>,
    },
    Tool(ToolCall),
    Diff(FileDiff),
    Permission(PermissionRequest),
    Compaction(ToolStatus),
    Subagent(Subagent),
    // how many files a finished turn changed, opening the first of them for review
    Changes(Changes),
    Question {
        questions: Vec<mux_core::protocol::Question>,
        answers: Option<Vec<Vec<String>>>,
        form: Option<mux_core::protocol::Form>,
    },
    // a proposed plan, and whether it's the one still waiting to be carried out
    Plan {
        text: String,
        actionable: bool,
    },
    Todos(Vec<mux_core::protocol::TodoStep>),
    // the model's reasoning, timed in unix milliseconds and still going while it has no end
    Reasoning {
        text: String,
        started: u64,
        ended: Option<u64>,
    },
    // the agent retrying requests to its model, counted on one row
    Retry {
        reason: String,
        attempt: u32,
        of: Option<u32>,
        status: ToolStatus,
    },
    Thinking,
}

#[derive(Debug)]
pub struct Changes {
    pub files: usize,
    pub added: usize,
    pub removed: usize,
    // the index of the turn's first edit card
    pub first: usize,
}

// a helper agent the main one started, with its own tool calls and final report
#[derive(Debug)]
pub struct Subagent {
    // the agent cli it runs in, whose mark it wears
    pub provider: mux_core::Provider,
    pub task: String,
    pub kind: String,
    pub status: ToolStatus,
    pub background: bool,
    // what it's doing right now, while it runs
    pub activity: String,
    pub steps: Vec<ToolCall>,
    pub result: String,
    pub tokens: u64,
}

#[derive(Debug)]
pub struct ToolCall {
    pub kind: ToolKind,
    pub target: String,
    pub status: ToolStatus,
    pub output: Option<String>,
    // left running in the background while the agent carried on
    pub background: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Edit,
    Run,
    Search,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Done,
    Failed,
}

#[derive(Debug)]
pub struct FileDiff {
    pub path: String,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug)]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffKind {
    Add,
    Remove,
    Context,
}

#[derive(Debug)]
pub struct PermissionRequest {
    pub title: String,
    pub detail: Option<String>,
    pub decision: Option<Decision>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    AlwaysAllow,
    Deny,
}

pub enum ChatAction {
    Send(String),
    // spend one of the account's banked limit resets, confirmed on the limit card
    UseReset,
    // the attach button was clicked, so the app should offer a file picker
    Attach,
    // a link to a file was clicked, with its target as the agent wrote it
    OpenLink(String),
    Stop,
    Compact,
    // the edit card at this index was clicked, to see its file's full diff
    OpenDiff(usize),
    // one of mux's own slash commands, by name
    Command(String),
    // the question at this index was answered, one list of picks per question
    Answer(usize, Vec<Vec<String>>),
    ImplementPlan,
    // the message at this index was picked to edit from, rewinding the chat to before it
    Rewind(usize),
    // something done to a waiting message, by its place in the queue
    Queue(usize, mux_core::protocol::QueueAction),
    // waiting messages, by their places in the queue, taken back into the composer to change
    EditQueued(Vec<usize>),
    Resume,
    Decide(usize, Decision),
    // a command left running in the background, by the id the agent gave it
    StopBackground(String),
}

// a banked limit reset the limit card can offer, with what became of the last one tried
#[derive(Clone, Debug, Default)]
pub struct ResetOffer {
    pub left: usize,
    pub pending: bool,
    pub note: Option<String>,
}

pub struct ChatView<'a> {
    session: u64,
    reset: Option<ResetOffer>,
    items: &'a [ChatItem],
    draft: &'a mut String,
    running: bool,
    controls: Option<Controls<'a>>,
    context: Option<ContextUsage>,
    attachments: Option<&'a mut Vec<String>>,
    dropping: bool,
    commands: &'a [SlashCommand],
    queue: &'a [mux_core::protocol::Queued],
    held: bool,
    background: &'a [mux_core::protocol::Background],
}

impl<'a> ChatView<'a> {
    pub fn new(items: &'a [ChatItem], draft: &'a mut String) -> Self {
        Self {
            reset: None,
            items,
            draft,
            session: 0,
            running: false,
            controls: None,
            context: None,
            attachments: None,
            dropping: false,
            commands: &[],
            queue: &[],
            held: false,
            background: &[],
        }
    }

    // keeps each session's scroll position, open steps and measured heights its own
    pub fn session(mut self, session: u64) -> Self {
        self.session = session;
        self
    }

    // commands the agent left running, shown above the composer
    pub fn background(mut self, tasks: &'a [mux_core::protocol::Background]) -> Self {
        self.background = tasks;
        self
    }

    // messages sent while the agent works, shown after the chat until they go out
    pub fn queue(mut self, queue: &'a [mux_core::protocol::Queued], held: bool) -> Self {
        self.queue = queue;
        self.held = held;
        self
    }

    pub fn commands(mut self, commands: &'a [SlashCommand]) -> Self {
        self.commands = commands;
        self
    }

    pub fn attachments(mut self, attachments: &'a mut Vec<String>) -> Self {
        self.attachments = Some(attachments);
        self
    }

    // files are being dragged over the window, so the chat shows where they'll go
    pub fn dropping(mut self, dropping: bool) -> Self {
        self.dropping = dropping;
        self
    }

    pub fn context(mut self, context: Option<ContextUsage>) -> Self {
        self.context = context;
        self
    }

    pub fn controls(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.controls = Some(Box::new(add));
        self
    }

    pub fn reset(mut self, reset: Option<ResetOffer>) -> Self {
        self.reset = reset;
        self
    }

    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Option<ChatAction> {
        let rect = ui.max_rect();
        let width = (rect.width() - space::LG * 2.0).min(MAX_WIDTH);
        let left = rect.center().x - width / 2.0;
        let mut action = None;

        // pinned to the bottom using last frame's height, since egui lays out top to bottom
        let height_id = ui.scope_id().with("composer-height");
        let height = ui.data(|d| d.get_temp::<f32>(height_id)).unwrap_or(96.0);
        let composer = Rect::from_min_max(
            pos2(left, rect.bottom() - space::MD - height),
            pos2(left + width, rect.bottom() - space::MD),
        );
        let mut bottom = ui.new_child(UiBuilder::new().max_rect(composer));
        // each list above the composer gets at most a quarter of the chat, so the chat itself stays in view
        let strip = (rect.height() / 4.0).max(cards::ROW * 3.0);
        if let Some(task) = background::strip(&mut bottom, self.background, strip) {
            action = Some(ChatAction::StopBackground(task));
        }
        if let Some(event) = queue::strip(&mut bottom, self.queue, self.held, strip) {
            use mux_core::protocol::QueueAction;
            action = Some(match event {
                queue::QueueEvent::SendNow(i) => ChatAction::Queue(i, QueueAction::SendNow),
                queue::QueueEvent::Remove(i) => ChatAction::Queue(i, QueueAction::Remove),
                queue::QueueEvent::Edit(i) => ChatAction::EditQueued(vec![i]),
                queue::QueueEvent::Resume => ChatAction::Resume,
            });
        }
        // what was sent before, oldest first, for stepping back through with the up key
        let history: Vec<&str> = self
            .items
            .iter()
            .filter_map(|item| match item {
                ChatItem::User {
                    text,
                    queued: false,
                    ..
                } if !text.trim().is_empty() => Some(text.as_str()),
                _ => None,
            })
            .collect();
        let mut input = Composer::new(self.draft)
            .running(self.running)
            .context(self.context)
            .commands(self.commands)
            .history(&history)
            .queued(!self.queue.is_empty());
        if let Some(attachments) = self.attachments {
            input = input.attachments(attachments);
        }
        if let Some(controls) = self.controls {
            input = input.controls(controls);
        }
        if let Some(composer) = input.show(&mut bottom) {
            action = Some(match composer {
                ComposerAction::Send(text) => ChatAction::Send(text),
                ComposerAction::Attach => ChatAction::Attach,
                ComposerAction::Stop => ChatAction::Stop,
                ComposerAction::Compact => ChatAction::Compact,
                ComposerAction::Command(name) => ChatAction::Command(name),
                ComposerAction::EditQueued => {
                    ChatAction::EditQueued((0..self.queue.len()).collect())
                }
            });
        }
        let measured = bottom.min_rect().height();
        if (measured - height).abs() > 0.5 {
            ui.data_mut(|d| d.insert_temp(height_id, measured));
            ui.ctx().request_discard("composer height changed");
        }
        let list = Rect::from_min_max(rect.min, pos2(rect.max.x, composer.top() - space::SM));

        let mut list_ui = ui.new_child(
            UiBuilder::new()
                .id_salt(("chat-list", self.session))
                .max_rect(list),
        );
        list_ui.set_clip_rect(list.intersect(ui.clip_rect()));
        drop_overlay(ui, rect, self.dropping);
        if self.items.is_empty() {
            empty(&mut list_ui);
            ui.allocate_rect(rect, Sense::hover());
            return action;
        }
        ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(&mut list_ui, |ui| {
                // center the column inside the scroll area so the scrollbar stays at the edge
                ui.horizontal_top(|ui| {
                    ui.add_space(left - rect.left());
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        ui.spacing_mut().item_spacing.y = space::MD;
                        ui.add_space(space::LG);
                        // each message gets its own word for the indicator, kept for the whole turn
                        let turns = self
                            .items
                            .iter()
                            .filter(|item| matches!(item, ChatItem::User { .. }))
                            .count();
                        let items = self.items;
                        let running = self.running;
                        let reset = self.reset.as_ref();
                        let show_items = |ui: &mut Ui, indices: &[usize]| {
                            show_items(ui, items, indices, running, turns, reset)
                        };
                        let mut plain = Vec::new();
                        let mut i = 0;
                        while i < items.len() {
                            plain.push(i);
                            let message = matches!(items[i], ChatItem::User { .. });
                            let turn = message.then(|| fold(items, i + 1, running));
                            let Some(turn) = turn.filter(|t| t.worth_folding(items)) else {
                                i += 1;
                                continue;
                            };
                            take(&mut action, show_items(ui, &plain));
                            plain.clear();
                            // a turn's steps fold behind one line once it's done, open while it runs
                            lazy(ui, ("turn", i), |ui| {
                                ui.push_id(("turn", i), |ui| {
                                    let id = ui.make_persistent_id("fold");
                                    let work: Vec<&ChatItem> =
                                        turn.work.iter().map(|&w| &items[w]).collect();
                                    let took = match &items[i] {
                                        ChatItem::User { took, .. } => *took,
                                        _ => None,
                                    };
                                    let label = cards::turn_summary(&work, turn.live, took);
                                    let (_, open) =
                                        cards::summary_row(ui, id, turn.live, turn.live, &label);
                                    crate::components::reveal(ui, id.with("steps"), open, |ui| {
                                        take(&mut action, show_items(ui, &turn.work));
                                    });
                                });
                            });
                            take(&mut action, show_items(ui, &turn.kept));
                            i = turn.end;
                        }
                        take(&mut action, show_items(ui, &plain));
                        ui.add_space(space::SM);
                    });
                });
            });

        ui.allocate_rect(rect, Sense::hover());
        action
    }
}

// the action of whatever was clicked, keeping the first one this frame
fn take(action: &mut Option<ChatAction>, shown: Option<ChatAction>) {
    if action.is_none() {
        *action = shown;
    }
}

// a turn's steps, which fold behind one line, and what stays in view when they're folded
struct Fold {
    work: Vec<usize>,
    kept: Vec<usize>,
    live: bool,
    // where the next message starts
    end: usize,
}

impl Fold {
    // a single step already shows as one line of its own, so folding it would only hide what it says.
    // the agent's messages don't count, since while it runs any of them may turn out to be the answer
    fn worth_folding(&self, items: &[ChatItem]) -> bool {
        self.work
            .iter()
            .filter(|&&i| !matches!(items[i], ChatItem::Assistant(_)))
            .count()
            >= 2
    }
}

// the items from `start` up to the next message, split into the steps that fold and what stays out
fn fold(items: &[ChatItem], start: usize, running: bool) -> Fold {
    let end = items[start..]
        .iter()
        .position(|item| matches!(item, ChatItem::User { .. }))
        .map_or(items.len(), |at| start + at);
    let live = running && end == items.len();
    // the answer to the message, which is its last reply once the turn is done
    let reply = (start..end)
        .rev()
        .find(|&i| matches!(&items[i], ChatItem::Assistant(_)))
        .filter(|_| !live);
    let (mut work, mut kept) = (Vec::new(), Vec::new());
    for (i, item) in items.iter().enumerate().take(end).skip(start) {
        // anything waiting on the person stays in view even while the turn runs
        let stays = match item {
            ChatItem::Permission(request) => request.decision.is_none(),
            ChatItem::Question { answers, .. } => answers.is_none(),
            ChatItem::Thinking | ChatItem::UsageLimit { .. } => true,
            ChatItem::Assistant(_) => Some(i) == reply,
            ChatItem::Error(_) => !live,
            ChatItem::Changes(_) | ChatItem::Plan { .. } => !live,
            _ => false,
        };
        if stays {
            kept.push(i);
        } else {
            work.push(i);
        }
    }
    Fold {
        work,
        kept,
        live,
        end,
    }
}

#[derive(Clone, Copy)]
struct Measured {
    width: f32,
    height: f32,
}

// draws a part of the chat only when it's near the screen and otherwise just keeps its room,
// because a long chat would spend every frame on what nobody can see
fn lazy(ui: &mut Ui, key: impl std::hash::Hash + std::fmt::Debug, add: impl FnOnce(&mut Ui)) {
    let id = ui.scope_id().with(("lazy", key));
    let width = ui.available_width();
    let top = ui.cursor().top();
    let clip = ui.clip_rect();
    let near = clip.expand2(egui::vec2(0.0, clip.height()));
    let known = ui
        .data(|d| d.get_temp::<Measured>(id))
        .filter(|m| m.width == width);
    if let Some(known) = known.filter(|m| top + m.height < near.top() || top > near.bottom()) {
        ui.add_space(known.height);
        return;
    }
    // egui numbers widgets by what came before them, and what came before may be skipped next frame
    ui.scope_builder(UiBuilder::new().scope_id(id), add);
    let height = ui.cursor().top() - top;
    ui.data_mut(|d| d.insert_temp(id, Measured { width, height }));
}

// the items at `indices` in order, with runs of tool calls among them grouped
fn show_items(
    ui: &mut Ui,
    items: &[ChatItem],
    indices: &[usize],
    running: bool,
    turns: usize,
    reset: Option<&ResetOffer>,
) -> Option<ChatAction> {
    let picked: Vec<&ChatItem> = indices.iter().map(|&i| &items[i]).collect();
    let mut action = None;
    let mut k = 0;
    while k < picked.len() {
        // consecutive tool calls are shown together as one activity group
        let steps = activity_steps(&picked[k..]);
        let tools = steps
            .iter()
            .filter(|s| matches!(s, ChatItem::Tool(_)))
            .count();
        let index = indices[k];
        if tools > 1 {
            lazy(ui, index, |ui| {
                ui.push_id(index, |ui| cards::activity(ui, &steps));
            });
            k += steps.len();
            continue;
        }
        lazy(ui, index, |ui| {
            let shown = ui.push_id(index, |ui| {
                show_item(ui, picked[k], index, running, turns, reset)
            });
            take(&mut action, shown.inner);
        });
        k += 1;
    }
    action
}

fn show_item(
    ui: &mut Ui,
    item: &ChatItem,
    index: usize,
    running: bool,
    turns: usize,
    reset: Option<&ResetOffer>,
) -> Option<ChatAction> {
    let mut action = None;
    match item {
        ChatItem::User {
            text,
            attachments,
            queued,
            ..
        } => {
            // a rewind waits for the agent to finish, so nothing is cut out from under it
            if cards::user_message(ui, text, attachments, *queued, !running) {
                action = Some(ChatAction::Rewind(index));
            }
        }
        ChatItem::Assistant(text) => {
            let shown = ui.scope(|ui| markdown(ui, text));
            if let Some(target) = shown.inner {
                action = Some(ChatAction::OpenLink(target));
            }
            let id = ui.make_persistent_id("reply-menu");
            crate::components::area_menu(ui, shown.response.rect, id, |ui| {
                crate::components::copy_item(ui, "Copy message", text);
            });
        }
        ChatItem::Error(message) => {
            let shown = cards::error(ui, message);
            let id = ui.make_persistent_id("error-menu");
            crate::components::area_menu(ui, shown.rect, id, |ui| {
                crate::components::copy_item(ui, "Copy error", message);
            });
        }
        ChatItem::Tool(tool) => cards::tool_call(ui, tool),
        ChatItem::UsageLimit { message, retry_at } => {
            if cards::usage_limit(ui, message, *retry_at, reset) {
                action = Some(ChatAction::UseReset);
            }
        }
        ChatItem::Diff(diff) => {
            if cards::diff(ui, diff) {
                action = Some(ChatAction::OpenDiff(index));
            }
        }
        ChatItem::Permission(request) => {
            if let Some(decision) = cards::permission(ui, request) {
                action = Some(ChatAction::Decide(index, decision));
            }
        }
        ChatItem::Compaction(status) => cards::compaction(ui, *status),
        ChatItem::Question {
            questions,
            answers,
            form,
        } => {
            if let Some(sent) = asks::question(ui, questions, answers.as_deref(), form.as_ref()) {
                action = Some(ChatAction::Answer(index, sent));
            }
        }
        ChatItem::Plan { text, actionable } => match asks::plan(ui, text, *actionable) {
            Some(asks::PlanAction::Implement) => action = Some(ChatAction::ImplementPlan),
            Some(asks::PlanAction::Link(target)) => action = Some(ChatAction::OpenLink(target)),
            None => {}
        },
        ChatItem::Todos(steps) => asks::todos(ui, steps),
        ChatItem::Changes(changes) => {
            if cards::changes(ui, changes) {
                action = Some(ChatAction::OpenDiff(changes.first));
            }
        }
        ChatItem::Subagent(agent) => {
            if let Some(target) = cards::subagent(ui, agent) {
                action = Some(ChatAction::OpenLink(target));
            }
        }
        ChatItem::Reasoning {
            text,
            started,
            ended,
        } => cards::reasoning(ui, text, *started, *ended),
        ChatItem::Retry {
            reason,
            attempt,
            of,
            status,
        } => cards::retry(ui, reason, *attempt, *of, *status),
        ChatItem::Thinking => cards::thinking(ui, turns),
    }
    action
}

// keeps a single thinking indicator, always at the end of the conversation
// the tool calls that run together from the start of `items`, taking in the finished thoughts between them,
// since an agent that reasons before each command would otherwise split every group apart
fn activity_steps<'a>(items: &[&'a ChatItem]) -> Vec<&'a ChatItem> {
    let mut steps = Vec::new();
    for (i, &item) in items.iter().enumerate() {
        let between = matches!(item, ChatItem::Reasoning { ended: Some(_), .. })
            && !steps.is_empty()
            && matches!(items.get(i + 1), Some(&&ChatItem::Tool(_)));
        if matches!(item, ChatItem::Tool(_)) || between {
            steps.push(item);
        } else {
            break;
        }
    }
    steps
}

pub fn set_thinking(items: &mut Vec<ChatItem>, thinking: bool) {
    items.retain(|item| !matches!(item, ChatItem::Thinking));
    // while a prompt waits on the user the agent isn't thinking, it's waiting
    let waiting = items
        .iter()
        .any(|item| matches!(item, ChatItem::Permission(r) if r.decision.is_none()));
    // reasoning still streaming already says the model is thinking
    let reasoning = matches!(items.last(), Some(ChatItem::Reasoning { ended: None, .. }));
    if thinking && !waiting && !reasoning {
        items.push(ChatItem::Thinking);
    }
}

// a soft panel over the chat while files are dragged over the window
fn drop_overlay(ui: &Ui, rect: Rect, dropping: bool) {
    let t = crate::motion::spring_bool(
        ui.ctx(),
        ui.scope_id().with("dropping"),
        dropping,
        crate::motion::Spring::QUICK,
    );
    if t <= 0.01 {
        return;
    }
    let p = theme::palette(ui);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        ui.scope_id().with("drop-overlay"),
    ));
    let area = rect.shrink(space::MD);
    let rounding = egui::CornerRadius::same(theme::radius::XL);
    painter.rect_filled(area, rounding, p.bg.gamma_multiply(0.85 * t));
    painter.rect_stroke(
        area,
        rounding,
        egui::Stroke::new(1.5, p.border_strong.gamma_multiply(t)),
        egui::StrokeKind::Inside,
    );
    let center = area.center();
    painter.text(
        center - egui::vec2(0.0, 14.0),
        egui::Align2::CENTER_CENTER,
        icons::PAPERCLIP,
        theme::font::icon(theme::icon::XL),
        p.text_muted.gamma_multiply(t),
    );
    painter.text(
        center + egui::vec2(0.0, 18.0),
        egui::Align2::CENTER_CENTER,
        "Drop files to attach them",
        theme::font::medium(theme::text::MD),
        p.text.gamma_multiply(t),
    );
}

fn empty(ui: &mut Ui) {
    EmptyState::new(icons::CHAT_CIRCLE, "Start a conversation")
        .detail("Ask the agent to plan, change or explain something in this project.")
        .show(ui);
}

// placeholder conversation for the gallery and app until agents are wired up
pub fn sample() -> Vec<ChatItem> {
    let tool = |kind, target: &str, status, output: Option<&str>| {
        ChatItem::Tool(ToolCall {
            kind,
            target: target.to_owned(),
            status,
            output: output.map(str::to_owned),
            background: false,
        })
    };
    let line = |kind, text: &str| DiffLine {
        kind,
        text: text.to_owned(),
    };
    vec![
        ChatItem::User {
            text: "Refactor the auth middleware to use the new session store, and add tests for expired sessions.".to_owned(),
            attachments: Vec::new(),
            queued: false,
            took: None,
        },
        ChatItem::Assistant(
            "I'll start by reading the current middleware and finding where sessions are loaded.".to_owned(),
        ),
        tool(
            ToolKind::Read,
            "src/middleware/auth.rs",
            ToolStatus::Done,
            Some("pub async fn auth(req: Request, next: Next) -> Response {\n    let cookie = req.headers().get(COOKIE);\n    let session = decode_cookie(cookie)?;\n    ..."),
        ),
        tool(
            ToolKind::Search,
            "SessionStore",
            ToolStatus::Done,
            Some("src/session/store.rs:12  pub struct SessionStore\nsrc/session/store.rs:40  impl SessionStore\nsrc/main.rs:28          let store = SessionStore::new(pool);"),
        ),
        ChatItem::Assistant(
            "The middleware decodes the cookie itself and never checks expiry. Here's the plan:\n\n- Move the lookup into `SessionStore::get`\n- Return **401** when a session has expired\n- Add tests for the valid and expired paths\n\n```rust\nlet session = store.get(&token).await?.ok_or(Unauthorized)?;\n```".to_owned(),
        ),
        ChatItem::Diff(FileDiff {
            path: "src/middleware/auth.rs".to_owned(),
            lines: vec![
                line(DiffKind::Context, "pub async fn auth(req: Request, next: Next) -> Response {"),
                line(DiffKind::Remove, "    let cookie = req.headers().get(COOKIE);"),
                line(DiffKind::Remove, "    let session = decode_cookie(cookie)?;"),
                line(DiffKind::Add, "    let token = session_token(&req)?;"),
                line(DiffKind::Add, "    let session = store.get(&token).await?.ok_or(Unauthorized)?;"),
                line(DiffKind::Add, "    if session.is_expired() {"),
                line(DiffKind::Add, "        return Err(Unauthorized);"),
                line(DiffKind::Add, "    }"),
                line(DiffKind::Context, "    req.extensions_mut().insert(session);"),
                line(DiffKind::Context, "    next.run(req).await"),
                line(DiffKind::Context, "}"),
                line(DiffKind::Context, ""),
                line(DiffKind::Add, "fn session_token(req: &Request) -> Result<String> {"),
                line(DiffKind::Add, "    cookie(req, \"sid\").ok_or(Unauthorized)"),
                line(DiffKind::Add, "}"),
            ],
        }),
        ChatItem::Permission(PermissionRequest {
            title: "Run a command?".to_owned(),
            detail: Some("cargo test -p auth".to_owned()),
            decision: None,
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatItem {
        ChatItem::User {
            text: text.into(),
            attachments: Vec::new(),
            queued: false,
            took: None,
        }
    }

    fn tool(kind: ToolKind, status: ToolStatus) -> ChatItem {
        ChatItem::Tool(ToolCall {
            kind,
            target: "x".into(),
            status,
            output: None,
            background: false,
        })
    }

    fn reply(text: &str) -> ChatItem {
        ChatItem::Assistant(text.into())
    }

    #[test]
    fn a_usage_limit_notice_stays_visible_when_other_work_is_folded() {
        let items = vec![
            user("go"),
            tool(ToolKind::Read, ToolStatus::Done),
            ChatItem::UsageLimit {
                message: "Limit reached".into(),
                retry_at: Some(100),
            },
        ];
        for running in [false, true] {
            let turn = fold(&items, 1, running);
            assert_eq!(turn.kept, [2]);
            assert_eq!(turn.work, [1]);
        }
    }

    #[test]
    fn a_finished_turn_keeps_only_its_answer_in_view() {
        let items = vec![
            user("fix it"),
            ChatItem::Reasoning {
                text: "hm".into(),
                started: 0,
                ended: Some(4000),
            },
            tool(ToolKind::Read, ToolStatus::Done),
            reply("Found it, editing now."),
            tool(ToolKind::Run, ToolStatus::Done),
            ChatItem::Error("the test runner crashed".into()),
            reply("Fixed, and the tests pass."),
            user("thanks"),
        ];
        let turn = fold(&items, 1, false);
        assert!(turn.worth_folding(&items));
        assert_eq!(turn.work, [1, 2, 3, 4]);
        assert_eq!(turn.kept, [5, 6], "the error and the answer");
        assert_eq!(turn.end, 7);
        let work: Vec<&ChatItem> = turn.work.iter().map(|&i| &items[i]).collect();
        assert_eq!(
            cards::turn_summary(&work, false, None),
            "Read 1 file · Ran 1 command"
        );
        assert_eq!(
            cards::turn_summary(&work, false, Some(192_000)),
            "Worked for 3m 12s · Read 1 file · Ran 1 command"
        );
        assert_eq!(
            cards::turn_summary(&work, false, Some(400)),
            "Read 1 file · Ran 1 command",
            "a turn over in a moment isn't worth timing"
        );
    }

    #[test]
    fn a_running_turn_folds_nothing_away_but_what_waits_on_the_person_stays_out() {
        let mut items = vec![
            user("go"),
            tool(ToolKind::Run, ToolStatus::Done),
            reply("Now the next step."),
            ChatItem::Permission(PermissionRequest {
                title: "Run a command?".into(),
                detail: None,
                decision: None,
            }),
        ];
        let turn = fold(&items, 1, true);
        assert!(turn.live);
        assert_eq!(turn.work, [1, 2], "the reply so far may not be the answer");
        assert_eq!(turn.kept, [3], "the approval is never folded away");

        items.push(ChatItem::Thinking);
        let turn = fold(&items, 1, true);
        assert_eq!(turn.kept, [3, 4]);
    }

    #[test]
    fn what_a_turn_leaves_behind_stays_in_view_once_its_done() {
        let items = vec![
            user("plan it"),
            tool(ToolKind::Search, ToolStatus::Done),
            ChatItem::Plan {
                text: "1. do it".into(),
                actionable: true,
            },
            ChatItem::Changes(Changes {
                files: 2,
                added: 3,
                removed: 1,
                first: 1,
            }),
        ];
        let turn = fold(&items, 1, false);
        assert_eq!(turn.work, [1]);
        assert_eq!(turn.kept, [2, 3]);
        // a question and answer with nothing in between has nothing to fold
        let chat = vec![user("hi"), reply("hello")];
        assert!(fold(&chat, 1, false).work.is_empty());
        // and one step already reads as a line of its own
        assert!(!turn.worth_folding(&items));
        let thought = ChatItem::Reasoning {
            text: String::new(),
            started: 0,
            ended: Some(1000),
        };
        let streaming = vec![user("hi"), thought, reply("Hel")];
        let live = fold(&streaming, 1, true);
        assert_eq!(live.work, [1, 2]);
        assert!(
            !live.worth_folding(&streaming),
            "a reply streaming in isn't a step"
        );
    }

    #[test]
    fn a_turn_with_only_thinking_says_how_long_it_thought() {
        let thought = ChatItem::Reasoning {
            text: String::new(),
            started: 1000,
            ended: Some(13000),
        };
        assert_eq!(
            cards::turn_summary(&[&thought], false, None),
            "Thought for 12s"
        );
        assert_eq!(
            cards::turn_summary(&[&thought], true, Some(9000)),
            "Working",
            "a running turn has no length yet"
        );
    }

    // how many vertices a chat needs drawn, which has to fit the gpu's 256 MB buffer at 20 bytes each
    fn vertices(
        items: &[ChatItem],
        queue: &[mux_core::protocol::Queued],
        background: &[mux_core::protocol::Background],
    ) -> usize {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        };
        let mut most = 0;
        // the first frames only measure, and animations and images settle over the ones after
        for _ in 0..3 {
            let mut output = ctx.run_ui(input(), |ui| {
                let mut draft = String::new();
                ChatView::new(items, &mut draft)
                    .queue(queue, false)
                    .background(background)
                    .show(ui);
            });
            output.textures_delta.clear();
            let count = ctx
                .tessellate(output.shapes, output.pixels_per_point)
                .iter()
                .map(|primitive| match &primitive.primitive {
                    egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
                    egui::epaint::Primitive::Callback(_) => 0,
                })
                .sum();
            most = usize::max(most, count);
        }
        most
    }

    // one huge unbroken line in every piece of text an agent controls, which once crashed the app
    #[test]
    fn no_text_from_an_agent_can_overflow_the_gpu() {
        use mux_core::protocol::{
            Background, BackgroundState, Question, QuestionOption, Queued, TodoStatus, TodoStep,
        };
        const LIMIT: usize = 1024 * 1024;
        let long = "x".repeat(1024 * 1024);
        let call = |target: &str, output: Option<&str>| ToolCall {
            kind: ToolKind::Run,
            target: target.to_owned(),
            status: ToolStatus::Done,
            output: output.map(str::to_owned),
            background: false,
        };
        let helper = |task: &str, kind: &str, activity: &str, result: &str, status| {
            ChatItem::Subagent(Subagent {
                provider: mux_core::Provider::Claude,
                task: task.to_owned(),
                kind: kind.to_owned(),
                status,
                background: false,
                activity: activity.to_owned(),
                steps: Vec::new(),
                result: result.to_owned(),
                tokens: 0,
            })
        };
        let question =
            |header: &str, question: &str, label: &str, description: &str| ChatItem::Question {
                questions: vec![Question {
                    id: "q".into(),
                    header: header.to_owned(),
                    question: question.to_owned(),
                    options: vec![QuestionOption {
                        label: label.to_owned(),
                        description: description.to_owned(),
                    }],
                    multiple: false,
                    other: false,
                    optional: false,
                }],
                answers: None,
                form: None,
            };
        let permission = |title: &str, detail: &str, decision| {
            ChatItem::Permission(PermissionRequest {
                title: title.to_owned(),
                detail: Some(detail.to_owned()),
                decision,
            })
        };
        let cases: Vec<(&str, ChatItem)> = vec![
            ("user", user(&long)),
            (
                "attachment",
                ChatItem::User {
                    text: "go".into(),
                    attachments: vec![long.clone()],
                    queued: false,
                    took: None,
                },
            ),
            ("reply", reply(&long)),
            ("code", reply(&format!("```\n{long}\n```"))),
            ("inline code", reply(&format!("`{long}`"))),
            ("heading", reply(&format!("# {long}"))),
            ("list", reply(&format!("- {long}"))),
            ("quote", reply(&format!("> {long}"))),
            ("link", reply(&format!("[{long}](https://example.com)"))),
            ("table head", reply(&format!("| {long} |\n|---|\n| b |"))),
            ("table cell", reply(&format!("| a |\n|---|\n| {long} |"))),
            ("error", ChatItem::Error(long.clone())),
            (
                "limit",
                ChatItem::UsageLimit {
                    message: long.clone(),
                    retry_at: None,
                },
            ),
            ("tool", ChatItem::Tool(call(&long, None))),
            ("tool output", ChatItem::Tool(call("x", Some(&long)))),
            (
                "diff",
                ChatItem::Diff(FileDiff {
                    path: "a".into(),
                    lines: vec![DiffLine {
                        kind: DiffKind::Add,
                        text: long.clone(),
                    }],
                }),
            ),
            (
                "diff path",
                ChatItem::Diff(FileDiff {
                    path: long.clone(),
                    lines: Vec::new(),
                }),
            ),
            ("asking title", permission(&long, "x", None)),
            ("asking detail", permission("x", &long, None)),
            (
                "decided title",
                permission(&long, "x", Some(Decision::Allow)),
            ),
            (
                "decided detail",
                permission("x", &long, Some(Decision::Allow)),
            ),
            ("helper task", helper(&long, "x", "", "", ToolStatus::Done)),
            ("helper kind", helper("x", &long, "", "", ToolStatus::Done)),
            (
                "helper activity",
                helper("x", "x", &long, "", ToolStatus::Running),
            ),
            (
                "helper result",
                helper("x", "x", "", &long, ToolStatus::Done),
            ),
            ("question header", question(&long, "x", "x", "x")),
            ("question", question("x", &long, "x", "x")),
            ("option", question("x", "x", &long, "x")),
            ("option text", question("x", "x", "x", &long)),
            (
                "plan",
                ChatItem::Plan {
                    text: long.clone(),
                    actionable: true,
                },
            ),
            (
                "todo",
                ChatItem::Todos(vec![TodoStep {
                    text: long.clone(),
                    status: TodoStatus::Pending,
                }]),
            ),
            (
                "reasoning",
                ChatItem::Reasoning {
                    text: long.clone(),
                    started: 0,
                    ended: Some(1),
                },
            ),
            (
                "retry",
                ChatItem::Retry {
                    reason: long.clone(),
                    attempt: 1,
                    of: None,
                    status: ToolStatus::Running,
                },
            ),
        ];
        let mut over = Vec::new();
        for (name, item) in cases {
            let items = vec![user("go"), item];
            let count = vertices(&items, &[], &[]);
            if count > LIMIT {
                over.push(format!("{name}: {count}"));
            }
        }
        let queued = [Queued {
            text: long.clone(),
            attachments: Vec::new(),
        }];
        let count = vertices(&[user("go")], &queued, &[]);
        if count > LIMIT {
            over.push(format!("queued message: {count}"));
        }
        let task = |label: &str, command: &str| Background {
            id: "b".into(),
            label: label.to_owned(),
            command: command.to_owned(),
            started: 0,
            state: BackgroundState::Running,
            output: String::new(),
            stoppable: false,
        };
        for (name, task) in [
            ("task label", task(&long, "x")),
            ("task command", task("x", &long)),
        ] {
            let count = vertices(&[user("go")], &[], &[task]);
            if count > LIMIT {
                over.push(format!("{name}: {count}"));
            }
        }
        assert!(over.is_empty(), "too many vertices to draw: {over:#?}");
    }

    // run with: cargo test -p mux-ui --release bench_ -- --ignored --nocapture
    // a chat with folded turns, loose steps and replies of very different heights
    fn mixed_chat(turns: usize, salt: usize) -> Vec<ChatItem> {
        let mut items = Vec::new();
        for turn in 0..turns {
            items.push(user(&format!("message {salt} {turn}")));
            if turn % 3 != 2 {
                for step in 0..3 {
                    items.push(ChatItem::Tool(ToolCall {
                        kind: ToolKind::Run,
                        target: format!("step {salt} {turn} {step}"),
                        status: ToolStatus::Done,
                        output: Some("line\n".repeat(20)),
                        background: false,
                    }));
                }
            }
            items.push(reply(
                &format!("Reply {salt} {turn}. **Done** with `code`, and enough words after that to wrap differently once the window gets narrower than it was before.\n\n")
                    .repeat(1 + (turn + salt) % 9),
            ));
        }
        items
    }

    struct Window {
        ctx: egui::Context,
        size: egui::Vec2,
        time: f64,
        session: u64,
    }

    impl Window {
        fn new() -> Self {
            let ctx = egui::Context::default();
            theme::install(&ctx);
            Self {
                ctx,
                size: egui::vec2(1400.0, 900.0),
                time: 0.0,
                session: 0,
            }
        }

        // the words on screen and where they are, and how many were drawn anywhere
        fn frame(
            &mut self,
            items: &[ChatItem],
            events: Vec<egui::Event>,
        ) -> (Vec<(i32, i32, String)>, usize) {
            self.time += 1.0 / 60.0;
            let screen = Rect::from_min_size(pos2(0.0, 0.0), self.size);
            let input = egui::RawInput {
                screen_rect: Some(screen),
                time: Some(self.time),
                predicted_dt: 1.0 / 60.0,
                events,
                ..Default::default()
            };
            let mut draft = String::new();
            let session = self.session;
            let mut output = self.ctx.run_ui(input, |ui| {
                ChatView::new(items, &mut draft).session(session).show(ui);
            });
            output.textures_delta.clear();
            let mut seen = Vec::new();
            let mut drawn = 0;
            for clipped in &output.shapes {
                let egui::Shape::Text(text) = &clipped.shape else {
                    continue;
                };
                drawn += 1;
                let rect = text.visual_bounding_rect().intersect(clipped.clip_rect);
                if rect.is_positive() && rect.intersects(screen) {
                    seen.push((
                        (text.pos.x * 4.0) as i32,
                        (text.pos.y * 4.0) as i32,
                        text.galley.job.text.clone(),
                    ));
                }
            }
            seen.sort();
            (seen, drawn)
        }

        fn settle(&mut self, items: &[ChatItem]) -> (Vec<(i32, i32, String)>, usize) {
            for _ in 0..90 {
                self.frame(items, Vec::new());
            }
            self.frame(items, Vec::new())
        }

        fn scroll(&mut self, items: &[ChatItem], by: f32) -> (Vec<(i32, i32, String)>, usize) {
            let events = vec![
                egui::Event::PointerMoved(pos2(self.size.x / 2.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, by),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers: Default::default(),
                },
            ];
            self.frame(items, events);
            self.settle(items)
        }

        // what the same frame looks like with every part of the chat drawn
        fn drawn_in_full(&mut self, items: &[ChatItem]) -> (Vec<(i32, i32, String)>, usize) {
            self.ctx.data_mut(|d| d.remove_by_type::<Measured>());
            self.frame(items, Vec::new())
        }

        fn click(&mut self, items: &[ChatItem], at: egui::Pos2) {
            let button = |pressed| egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            self.frame(items, vec![egui::Event::PointerMoved(at)]);
            self.frame(items, vec![button(true)]);
            self.frame(items, vec![button(false)]);
        }
    }

    #[test]
    fn skipping_what_is_off_screen_changes_nothing_on_screen() {
        let items = mixed_chat(120, 0);
        let mut window = Window::new();
        let (bottom, _) = window.settle(&items);
        let mut skipped = 0;
        let mut views = vec![bottom];
        // far enough to pass through the whole chat in both directions
        for by in [
            700.0, 2500.0, 6000.0, 400.0, 90000.0, -3000.0, -500.0, -90000.0, 2000.0,
        ] {
            let (lazily, drawn) = window.scroll(&items, by);
            let (fully, all) = window.drawn_in_full(&items);
            assert_eq!(lazily, fully, "after scrolling by {by}");
            assert!(!lazily.is_empty());
            if drawn * 2 < all {
                skipped += 1;
            }
            views.push(lazily);
        }
        assert!(skipped >= 5, "off-screen parts were drawn anyway");
        views.dedup();
        assert!(views.len() >= 6, "the scrolling never moved the chat");
    }

    #[test]
    fn opening_a_step_or_resizing_keeps_the_chat_in_place() {
        let items = mixed_chat(120, 0);
        let mut window = Window::new();
        window.settle(&items);
        let (before, _) = window.scroll(&items, 6000.0);
        // a turn's fold row, the way a person would open one
        let label = cards::turn_summary(&[&items[1], &items[2], &items[3]], false, None);
        let (x, y, _) = before
            .iter()
            .find(|(_, _, text)| *text == label)
            .expect("a folded turn on screen")
            .clone();
        window.click(&items, pos2(x as f32 / 4.0 + 4.0, y as f32 / 4.0 + 8.0));
        let (opened, _) = window.settle(&items);
        assert_ne!(opened, before, "the click opened nothing");
        assert_eq!(opened, window.drawn_in_full(&items).0, "after opening");
        window.settle(&items);
        window.size = egui::vec2(420.0, 800.0);
        let (narrow, _) = window.settle(&items);
        assert_eq!(narrow, window.drawn_in_full(&items).0, "after narrowing");
        let (scrolled, _) = window.scroll(&items, 3000.0);
        assert_eq!(
            scrolled,
            window.drawn_in_full(&items).0,
            "scrolled when narrow"
        );
    }

    #[test]
    fn a_reply_growing_off_screen_can_be_scrolled_to_in_one_go() {
        let mut items = mixed_chat(120, 0);
        let mut window = Window::new();
        window.settle(&items);
        window.scroll(&items, 9000.0);
        let last = items.len() - 1;
        for _ in 0..200 {
            if let ChatItem::Assistant(text) = &mut items[last] {
                text.push_str("\n\nmore text arriving");
            }
            window.frame(&items, Vec::new());
        }
        if let ChatItem::Assistant(text) = &mut items[last] {
            text.push_str("\n\nthe very end");
        }
        window.frame(&items, Vec::new());
        let (bottom, _) = window.scroll(&items, -900000.0);
        assert!(bottom.iter().any(|(_, _, text)| text == "the very end"));
        assert_eq!(bottom, window.drawn_in_full(&items).0);
    }

    #[test]
    fn each_session_measures_its_own_chat() {
        let (first, second) = (mixed_chat(120, 0), mixed_chat(120, 4));
        let mut window = Window::new();
        window.settle(&first);
        window.session = 1;
        window.settle(&second);
        let (lazily, _) = window.scroll(&second, 6000.0);
        assert_eq!(lazily, window.drawn_in_full(&second).0);
        window.session = 0;
        let (back, _) = window.settle(&first);
        assert!(back.iter().any(|(_, _, text)| text.contains("Reply 0 ")));
        assert_eq!(back, window.drawn_in_full(&first).0);
    }

    #[test]
    fn a_selection_survives_scrolling_while_it_stays_on_screen() {
        use egui::text_selection::LabelSelectionState;
        let items = mixed_chat(120, 0);
        let mut window = Window::new();
        window.settle(&items);
        let (view, _) = window.scroll(&items, 9000.0);
        // near the top, so scrolling up moves it down the screen and keeps it in view
        let (x, y, _) = view
            .iter()
            .filter(|(_, y, text)| text.starts_with("Reply") && *y > 100 * 4)
            .min_by_key(|(_, y, _)| *y)
            .expect("a reply on screen")
            .clone();
        let (from, to) = (
            pos2(x as f32 / 4.0 + 2.0, y as f32 / 4.0 + 8.0),
            pos2(x as f32 / 4.0 + 90.0, y as f32 / 4.0 + 8.0),
        );
        let button = |pressed, pos| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        window.frame(&items, vec![egui::Event::PointerMoved(from)]);
        window.frame(&items, vec![button(true, from)]);
        window.frame(&items, vec![egui::Event::PointerMoved(to)]);
        window.frame(&items, vec![button(false, to)]);
        let selected = |window: &Window| {
            window
                .ctx
                .plugin::<LabelSelectionState>()
                .lock()
                .has_selection()
        };
        assert!(selected(&window), "the drag selected nothing");
        let mut last = view;
        for _ in 0..4 {
            let (moved, _) = window.scroll(&items, 100.0);
            assert_ne!(moved, last, "the chat didn't scroll");
            assert!(selected(&window), "scrolling lost the selection");
            last = moved;
        }
    }

    #[test]
    #[ignore]
    fn bench_drawing_a_long_chat() {
        let mut items = Vec::new();
        for turn in 0..200 {
            items.push(user(&format!("message {turn}")));
            for _ in 0..10 {
                items.push(ChatItem::Tool(ToolCall {
                    kind: ToolKind::Run,
                    target: "cargo test --workspace".into(),
                    status: ToolStatus::Done,
                    output: Some("test result: ok\n".repeat(1600)),
                    background: false,
                }));
            }
            items.push(ChatItem::Diff(FileDiff {
                path: "/tmp/src/lib.rs".into(),
                lines: (0..200)
                    .map(|_| DiffLine {
                        kind: DiffKind::Add,
                        text: "let a = 1;".into(),
                    })
                    .collect(),
            }));
            items.push(reply(
                &"Fixed it. Here's **what** changed:\n\n- one\n- two\n\n".repeat(20),
            ));
        }
        let ctx = egui::Context::default();
        theme::install(&ctx);
        let input = || egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                pos2(0.0, 0.0),
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        };
        let mut draft = String::new();
        let mut frame = |label: &str| {
            let start = std::time::Instant::now();
            let mut output = ctx.run_ui(input(), |ui| {
                ChatView::new(&items, &mut draft).show(ui);
            });
            let laid_out = start.elapsed();
            output.textures_delta.clear();
            let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
            println!(
                "{label}: {laid_out:?} to lay out, {:?} in all, {} meshes",
                start.elapsed(),
                meshes.len()
            );
        };
        frame("first frame");
        frame("second frame");
        for _ in 0..3 {
            frame("settled frame");
        }
        let mut window = Window::new();
        window.settle(&items);
        window.frame(&items, Vec::new());
        let mut worst = std::time::Duration::ZERO;
        for _ in 0..600 {
            let wheel = vec![
                egui::Event::PointerMoved(pos2(700.0, 300.0)),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Point,
                    delta: egui::vec2(0.0, 400.0),
                    phase: egui::TouchPhase::Move,
                    source: egui::MouseWheelSource::Unknown,
                    modifiers: Default::default(),
                },
            ];
            let start = std::time::Instant::now();
            window.frame(&items, wheel);
            worst = worst.max(start.elapsed());
        }
        println!("scrolling: worst frame {worst:?}");
    }
}
