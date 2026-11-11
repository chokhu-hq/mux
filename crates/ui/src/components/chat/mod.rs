mod asks;
mod attachments;
mod background;
mod cards;
mod composer;
mod markdown;
mod queue;
mod slash;

use std::sync::Arc;

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
    // the chat carried on with another agent from here
    Switched {
        from: String,
        to: String,
    },
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
    // pictures the tool gave back, as paths on disk
    pub images: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolKind {
    Read,
    Edit,
    Run,
    Search,
    // a tool from an mcp server
    Mcp,
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
    trailing: Option<Controls<'a>>,
    context: Option<ContextUsage>,
    attachments: Option<&'a mut Vec<String>>,
    dropping: bool,
    commands: &'a [SlashCommand],
    files: Option<&'a Arc<[String]>>,
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
            trailing: None,
            context: None,
            attachments: None,
            dropping: false,
            commands: &[],
            files: None,
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

    pub fn files(mut self, files: &'a Arc<[String]>) -> Self {
        self.files = Some(files);
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

    // controls on the right beside send, like the model picker
    pub fn trailing(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.trailing = Some(Box::new(add));
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
            .files_from(self.files)
            .history(&history)
            .queued(!self.queue.is_empty());
        if let Some(attachments) = self.attachments {
            input = input.attachments(attachments);
        }
        if let Some(trailing) = self.trailing {
            input = input.trailing(trailing);
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
            // pictures a tool gave back are usually what the turn was for
            ChatItem::Tool(tool) => !tool.images.is_empty(),
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
        ChatItem::Switched { from, to } => cards::switched(ui, from, to),
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
            images: Vec::new(),
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
mod tests;
