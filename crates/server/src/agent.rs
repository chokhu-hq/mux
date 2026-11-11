use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine;
use mux_core::protocol::{
    AgentItem, AgentSettings, AgentSummary, ContextUsage, Decision, FileChange, ItemStatus,
    McpServer, McpState, QueueAction, Queued, Skill, UsageWindow, is_image,
};
use mux_core::{Access, Provider, SessionStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::{Event, usage};

mod background;
mod claude;
mod codex;
mod group;
mod interaction;
mod launch;
mod opencode;
mod refresh;
mod rewind;
mod subagent;
mod switch;

pub(crate) use claude::forget_parent_claude;
use claude::{claude_answer, claude_args, claude_content};
use codex::{add_skills, turn_params};
pub(crate) use refresh::stop_claude;

// long command output is cut down to its tail, which is the part people read
const OUTPUT_LIMIT: usize = 32 * 1024;
// how much of an agent cli's stderr makes it into the log for each launch
const STDERR_LINES: usize = 200;
// the ids of the control requests that change a running claude's settings start with this
const CONFIG_REQUEST: &str = "mux-config-";

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    thread_id: Option<String>,
    // claude's session, which --resume picks back up
    #[serde(default)]
    session_id: Option<String>,
    items: Vec<AgentItem>,
    #[serde(default)]
    settings: AgentSettings,
    #[serde(default)]
    context: Option<ContextUsage>,
    #[serde(default)]
    commands: Vec<String>,
    #[serde(default)]
    claude_head: Option<String>,
    #[serde(default)]
    resume_at: Option<String>,
    #[serde(default)]
    pending_revert: Option<String>,
    #[serde(default)]
    queue: Vec<Queued>,
    #[serde(default)]
    resumes: u32,
    #[serde(default)]
    handover: switch::Handover,
}

enum Call {
    Initialize,
    StartThread,
    ResumeThread,
    StartTurn,
    // winding codex's thread back for a rewound conversation
    Revert,
    // input added to a running turn, kept so it can start its own turn if that one already ended
    Steer(String, Vec<String>),
    Interrupt,
    Compact,
    // an opencode session setting, like its model or mode
    Configure(&'static str, String),
}

struct Process {
    stdin: UnboundedSender<String>,
    // held so the process is killed when the agent is dropped or restarted, and absent in tests
    child: Option<Child>,
    claude: bool,
    // stops everything the agent started once it's dropped
    group: Option<group::Group>,
}

impl Drop for Process {
    fn drop(&mut self) {
        let child = self.child.take();
        let group = self.group.take();
        // claude may be refreshing its sign-in, so it's stopped once that's done rather than straight away
        if self.claude
            && child.is_some()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            runtime.spawn(async move {
                refresh::refresh_done().await;
                drop(child);
                drop(group);
            });
        }
    }
}

#[cfg(test)]
impl Process {
    fn fake(stdin: UnboundedSender<String>) -> Self {
        Self {
            stdin,
            child: None,
            claude: false,
            group: None,
        }
    }
}

// an approval the person hasn't answered yet, with what claude needs echoed back on allow
struct Pending {
    request: Value,
    index: usize,
    input: Value,
    suggestions: Value,
}

// one coding agent conversation, driven over stdio through either `codex app-server`
// or claude's stream-json mode, with both mapped onto the same transcript
// what changed in a conversation since the server last asked
pub struct Changes {
    pub items: Vec<(usize, AgentItem)>,
    pub appends: Vec<(usize, usize, String)>,
    pub busy: Option<bool>,
}

pub struct Agent {
    pub generation: u64,
    pub items: Vec<AgentItem>,
    pub busy: bool,
    pub settings: AgentSettings,
    // the latest rate limits the cli reported, taken by the server to pass on
    pub usage: Option<Vec<UsageWindow>>,
    pub context: Option<ContextUsage>,
    context_changed: bool,
    // the skill names claude last reported, which include plugin skills that aren't files mux can read
    pub commands: Vec<String>,
    // the skills found on disk or listed by codex, set by the server when the chat opens
    found: Vec<Skill>,
    commands_changed: bool,
    thread_id: Option<String>,
    thread_live: bool,
    turn_id: Option<String>,
    cwd: Option<PathBuf>,
    process: Option<Process>,
    session_id: Option<String>,
    // the assistant reply claude is streaming into, if any
    live_text: Option<usize>,
    // the reasoning still being streamed, which ends when anything else arrives
    live_reasoning: Option<usize>,
    // the settings claude was launched with, since changing them means relaunching
    launched: Option<AgentSettings>,
    queued: Vec<(String, Vec<String>)>,
    next_call: u64,
    calls: HashMap<u64, Call>,
    by_codex_id: HashMap<String, usize>,
    approvals: HashMap<u64, Pending>,
    next_approval: u64,
    changed: BTreeSet<usize>,
    // streamed text added to items since the last changes were taken, with where in each item's text it starts
    appends: BTreeMap<usize, (usize, String)>,
    busy_changed: bool,
    // messages claude hasn't echoed back yet, which it may fold into one turn when they arrive close together
    claude_waiting: VecDeque<String>,
    // the messages written to this claude, kept to send again if it has to start over
    claude_sent: Vec<Value>,
    // claude no longer had the conversation it was resumed into, so it starts a fresh one once it exits
    claude_lost: bool,
    // the retry row of the turn in progress, while the agent is still trying again
    live_retry: Option<usize>,
    // the message that started the turn in progress, and when, for timing the turn once it ends
    turn_started: Option<(usize, u64)>,
    // claude is mid turn, whether answering a message or one it started itself, like a background subagent waking
    claude_turn: bool,
    // a stop was sent this turn, so the error result claude ends it with is expected
    claude_stopping: bool,
    // a subagent's own tool calls by id, as its card and the step within it
    sub_steps: HashMap<String, (usize, usize)>,
    // claude's subagent task ids and codex's subagent threads, each pointing at its card
    tasks: HashMap<String, usize>,
    child_threads: HashMap<String, usize>,
    // questions the agent is waiting on an answer to, by the id shown in the chat
    questions: HashMap<u64, interaction::Asked>,
    // this turn's checklist, updated in place as the agent works through it
    todos: Option<usize>,
    // codex's default model, which plan mode needs named when none was picked
    pub default_model: Option<String>,
    // the mcp servers mux adds, and the ones the running cli was started with
    pub mcp: Vec<McpServer>,
    started_mcp: Vec<McpServer>,
    // how mux's servers started since the server last asked, and which ones already said they failed this launch
    mcp_states: Vec<(String, McpState)>,
    mcp_failed: HashSet<String>,
    // claude's latest reply, which a message sent now would be rewound to
    claude_head: Option<String>,
    // where claude's next launch picks the session up, after a rewind
    resume_at: Option<String>,
    // a rewind for codex waiting for its thread to open
    pending_revert: Option<String>,
    // messages sent while the agent worked, waiting with mux until it's free
    pub queue: Vec<Queued>,
    // how many times in a row mux carried on a turn a restart cut off, without the user saying anything since
    pub resumes: u32,
    // when the usage limit that blocked the turn in progress resets, in unix seconds
    limit_reset: Option<u64>,
    // the agent's latest limit windows, which say when a blocked codex can go again
    windows: Vec<UsageWindow>,
    // a turn a usage limit stopped, to carry on at this time, taken by the server to wake it then
    pause: Option<u64>,
    // the agent's own conversation ids as last saved, so a new one is saved straight away
    saved_ids: (Option<String>, Option<String>),
    // a stop holds the queue back until the person sends again or resumes it
    pub held: bool,
    queue_changed: bool,
    // the compaction item still running, whether asked for or started by the agent itself
    compacting: Option<usize>,
    compact_queued: bool,
    applied: opencode::Applied,
    pending_prompt: Option<Value>,
    stop_requested: bool,
    // commands the agent left running in the background, and ones that finished since the last message
    background: Vec<background::Tracked>,
    background_changed: bool,
    // where background output is reported, kept from the launch for the followers it starts
    events: Option<UnboundedSender<Event>>,
    session: u64,
    // other agents' conversations in this chat, and history owed to the one carrying it on
    handover: switch::Handover,
}

impl Agent {
    pub fn load(path: &Path) -> Self {
        let saved: Saved = match fs::read(path).map(|bytes| serde_json::from_slice(&bytes)) {
            Ok(Ok(saved)) => saved,
            Ok(Err(err)) => {
                // set aside rather than saved over, since a newer build may have written something this one can't read
                let backup = path.with_extension(format!("json.bad-{}", crate::store::now()));
                log::error!(
                    "{} is unreadable ({err}), moved it to {}",
                    path.display(),
                    backup.display()
                );
                let _ = fs::rename(path, backup);
                Saved::default()
            }
            Err(_) => Saved::default(),
        };
        let ids = (saved.thread_id.clone(), saved.session_id.clone());
        let mut items = saved.items;
        // nothing from a previous process is still running or waiting on an answer
        for item in &mut items {
            match item {
                AgentItem::Command { status, .. }
                | AgentItem::FileChange { status, .. }
                | AgentItem::Compaction { status }
                | AgentItem::Subagent { status, .. }
                    if *status == ItemStatus::Running =>
                {
                    *status = ItemStatus::Failed
                }
                AgentItem::Approval { decision, .. } if decision.is_none() => {
                    *decision = Some(Decision::Deny)
                }
                AgentItem::User { queued, .. } => *queued = false,
                AgentItem::Question { answers, .. } if answers.is_none() => {
                    *answers = Some(Vec::new())
                }
                _ => {}
            }
        }
        Self {
            generation: 0,
            items,
            busy: false,
            settings: saved.settings,
            usage: None,
            context: saved.context,
            context_changed: false,
            commands: saved.commands,
            found: Vec::new(),
            commands_changed: false,
            thread_id: saved.thread_id,
            thread_live: false,
            turn_id: None,
            cwd: None,
            process: None,
            session_id: saved.session_id,
            live_text: None,
            live_reasoning: None,
            launched: None,
            queued: Vec::new(),
            next_call: 0,
            calls: HashMap::new(),
            by_codex_id: HashMap::new(),
            approvals: HashMap::new(),
            next_approval: 0,
            changed: BTreeSet::new(),
            appends: BTreeMap::new(),
            busy_changed: false,
            claude_waiting: VecDeque::new(),
            claude_sent: Vec::new(),
            claude_lost: false,
            live_retry: None,
            turn_started: None,
            claude_turn: false,
            claude_stopping: false,
            sub_steps: HashMap::new(),
            tasks: HashMap::new(),
            child_threads: HashMap::new(),
            questions: HashMap::new(),
            todos: None,
            default_model: None,
            mcp: Vec::new(),
            started_mcp: Vec::new(),
            mcp_states: Vec::new(),
            mcp_failed: HashSet::new(),
            claude_head: saved.claude_head,
            resume_at: saved.resume_at,
            pending_revert: saved.pending_revert,
            // nothing runs a saved queue until the person asks, since the agent it was meant for has gone
            held: !saved.queue.is_empty(),
            queue: saved.queue,
            resumes: saved.resumes,
            handover: saved.handover,
            limit_reset: None,
            windows: Vec::new(),
            pause: None,
            saved_ids: ids,
            queue_changed: false,
            compacting: None,
            compact_queued: false,
            applied: opencode::Applied::default(),
            pending_prompt: None,
            stop_requested: false,
            background: Vec::new(),
            background_changed: false,
            events: None,
            session: 0,
        }
    }

    fn set_context(&mut self, context: ContextUsage) {
        if self.context != Some(context) {
            self.context = Some(context);
            self.context_changed = true;
        }
    }

    pub fn set_skills(&mut self, found: Vec<Skill>) {
        if found != self.found {
            self.found = found;
            self.commands_changed = true;
        }
    }

    // every skill on offer: for claude its own list decides the names, and the files found supply descriptions,
    // matched with or without the `plugin:` prefix claude gives skills that came from a plugin or an account
    pub fn skills(&self) -> Vec<Skill> {
        if self.settings.provider != Provider::Claude || self.commands.is_empty() {
            return self.found.clone();
        }
        let mut skills: Vec<Skill> = self
            .commands
            .iter()
            .map(|name| {
                let short = name.rsplit(':').next().unwrap_or(name);
                match self
                    .found
                    .iter()
                    .find(|s| s.name == *name || s.name == short)
                {
                    Some(found) => Skill {
                        name: name.clone(),
                        ..found.clone()
                    },
                    None => Skill {
                        name: name.clone(),
                        description: String::new(),
                        path: String::new(),
                        scope: "Plugin".to_owned(),
                    },
                }
            })
            .collect();
        // a skill added since claude last started isn't in its list yet, but will be once it restarts
        for found in &self.found {
            let listed = skills.iter().any(|s| {
                s.name == found.name || s.name.rsplit(':').next() == Some(found.name.as_str())
            });
            if !listed {
                skills.push(found.clone());
            }
        }
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        skills
    }

    // the skills when they changed since the last call
    pub fn take_skills(&mut self) -> Option<Vec<Skill>> {
        std::mem::take(&mut self.commands_changed).then(|| self.skills())
    }

    // one of mux's servers started or failed, which a failure also says in the chat, once each launch
    fn mcp_started(&mut self, name: &str, state: McpState) {
        if !self.started_mcp.iter().any(|s| s.enabled && s.name == name) {
            return;
        }
        if let McpState::Failed(reason) = &state
            && self.mcp_failed.insert(name.to_owned())
        {
            let reason = reason.trim();
            let message = if reason.is_empty() {
                format!(
                    "The MCP server {name} couldn't start, so its tools aren't available. Check its command in Settings, under MCP servers."
                )
            } else {
                format!(
                    "The MCP server {name} couldn't start, so its tools aren't available. {reason}"
                )
            };
            self.push(AgentItem::Error { message });
        }
        self.mcp_states.push((name.to_owned(), state));
    }

    pub fn take_mcp_states(&mut self) -> Vec<(String, McpState)> {
        std::mem::take(&mut self.mcp_states)
    }

    pub fn is_running(&self) -> bool {
        self.process.is_some()
    }

    // the context usage when it changed since the last call, for the server to pass on
    pub fn take_context(&mut self) -> Option<ContextUsage> {
        std::mem::take(&mut self.context_changed)
            .then_some(self.context)
            .flatten()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let saved = Saved {
            thread_id: self.thread_id.clone(),
            session_id: self.session_id.clone(),
            items: self.items.clone(),
            settings: self.settings.clone(),
            context: self.context,
            commands: self.commands.clone(),
            claude_head: self.claude_head.clone(),
            resume_at: self.resume_at.clone(),
            pending_revert: self.pending_revert.clone(),
            queue: self.queue.clone(),
            resumes: self.resumes,
            handover: self.handover.clone(),
        };
        let tmp = path.with_extension("json.tmp");
        // private, since a chat holds the person's prompts, code and command output
        mux_core::write_private(&tmp, &serde_json::to_vec(&saved).map_err(io::Error::other)?)?;
        fs::rename(tmp, path)
    }

    // a short account of the conversation for the sidebar, where a diff's lines say what changed
    pub fn summary(&self) -> AgentSummary {
        let mut summary = AgentSummary {
            model: self.settings.model.clone().unwrap_or_default(),
            context: self.context,
            ..AgentSummary::default()
        };
        let mut files = Vec::new();
        for item in &self.items {
            match item {
                AgentItem::User { .. } => summary.messages += 1,
                AgentItem::Assistant { text } if !text.trim().is_empty() => {
                    summary.preview = text.chars().take(240).collect();
                }
                AgentItem::FileChange { changes, .. } => {
                    for change in changes {
                        if !files.contains(&change.path) {
                            files.push(change.path.clone());
                        }
                        for line in change.diff.lines() {
                            if line.starts_with('+') && !line.starts_with("+++") {
                                summary.added += 1;
                            } else if line.starts_with('-') && !line.starts_with("---") {
                                summary.removed += 1;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        summary.files = files.len() as u32;
        summary
    }

    pub fn status(&self) -> SessionStatus {
        if !self.approvals.is_empty() || !self.questions.is_empty() {
            SessionStatus::Waiting
        } else if self.busy {
            SessionStatus::Running
        } else if self.items.is_empty() {
            SessionStatus::Idle
        } else {
            SessionStatus::Done
        }
    }

    // the items and busy flag that changed since the last call, for pushing to clients
    pub fn take_changes(&mut self) -> Changes {
        let changed = std::mem::take(&mut self.changed);
        // an item sent whole already carries whatever was streamed into it
        let appends = std::mem::take(&mut self.appends)
            .into_iter()
            .filter(|(index, _)| !changed.contains(index))
            .map(|(index, (at, text))| (index, at, text))
            .collect();
        let items = changed
            .into_iter()
            .filter_map(|i| Some((i, self.items.get(i)?.clone())))
            .collect();
        let busy = std::mem::take(&mut self.busy_changed).then_some(self.busy);
        Changes {
            items,
            appends,
            busy,
        }
    }

    // a usage limit stopped the turn, so it carries on by itself once the limit resets
    fn pause_until(&mut self, reset: u64, message: String) {
        self.usage_limit(message, Some(reset));
        self.pause = Some(reset);
    }

    fn usage_limit(&mut self, message: String, retry_at: Option<u64>) {
        let duplicate = self.items.last().is_some_and(|item| match item {
            AgentItem::Assistant { text } => text.trim() == message.trim(),
            AgentItem::UsageLimit {
                message: previous, ..
            } => previous.trim() == message.trim(),
            _ => false,
        });
        let item = AgentItem::UsageLimit { message, retry_at };
        if duplicate {
            let index = self.items.len() - 1;
            self.items[index] = item;
            self.changed.insert(index);
        } else {
            self.push(item);
        }
    }

    pub fn take_pause(&mut self) -> Option<u64> {
        self.pause.take()
    }

    // whether the agent's conversation id changed since this was last asked, which has to be saved at once,
    // since a crash before the turn ends would otherwise leave nothing to resume it with
    pub fn ids_changed(&mut self) -> bool {
        let ids = (self.thread_id.clone(), self.session_id.clone());
        if ids == self.saved_ids {
            return false;
        }
        self.saved_ids = ids;
        true
    }

    // streamed text joins an item's text, and is sent on as just the new part rather than the whole item again
    fn append(&mut self, index: usize, chunk: &str) {
        let Some(text) = self.items.get_mut(index).and_then(AgentItem::streamed_text) else {
            return;
        };
        let at = text.len();
        text.push_str(chunk);
        self.appends
            .entry(index)
            .or_insert_with(|| (at, String::new()))
            .1
            .push_str(chunk);
    }

    pub fn send(
        &mut self,
        text: String,
        attachments: Vec<String>,
        session: u64,
        generation: u64,
        cwd: PathBuf,
        events: &UnboundedSender<Event>,
    ) {
        // while the agent works a message waits with mux, where it can still be changed, until its turn
        if self.busy && self.process.is_some() {
            self.queue.push(Queued { text, attachments });
            self.held = false;
            self.queue_changed = true;
            return;
        }
        self.held = false;
        // mcp servers are only read when the cli starts, so an idle one starts again to pick up a change.
        // one with commands or helpers still going in the background waits, since starting again would end them
        if self.process.is_some() && self.started_mcp != self.mcp && !self.working() {
            let held = self.held;
            self.exited();
            // the queue was just let go by this send, and starting again shouldn't hold it back
            self.held = held;
        }
        self.record(&text, &attachments, false);
        if !self.launch(false, session, generation, cwd, events) {
            return;
        }
        self.transmit(text, attachments, false);
    }

    // puts the message in the transcript, noting where the agent's history stood so it can be rewound to here
    fn record(&mut self, text: &str, attachments: &[String], steer: bool) {
        // a new message starts a new checklist
        self.todos = None;
        self.clear_finished_background();
        let claude = self.settings.provider == Provider::Claude;
        // codex waits for its thread to open before it takes a message
        let queued = !claude && !self.thread_live && self.process.is_some();
        // claude's latest reply, or the codex turn a steered message joins, filled in once it starts otherwise
        let rewind = if claude {
            Some(self.claude_head.clone().unwrap_or_default())
        } else if steer {
            self.turn_id.clone()
        } else {
            None
        };
        // a message that joins a turn already running doesn't start a new one to time
        let starts = !self.busy;
        let index = self.push(AgentItem::User {
            text: text.to_owned(),
            attachments: attachments.to_vec(),
            queued,
            rewind,
            took: None,
        });
        if starts {
            self.turn_started = Some((index, now_ms()));
        }
        self.set_busy(true);
    }

    fn transmit(&mut self, text: String, attachments: Vec<String>, steer: bool) {
        let text = if steer { text } else { self.with_history(text) };
        if self.settings.provider == Provider::Claude {
            let content = claude_content(&text, &attachments, &self.skills());
            // claude echoes the text as sent, with skills and attachments rewritten, so that's what is matched later
            let sent = content
                .as_array()
                .and_then(|blocks| blocks.last())
                .and_then(|block| block["text"].as_str())
                .unwrap_or_default()
                .to_owned();
            let mut line = json!({
                "type": "user",
                "message": { "role": "user", "content": content }
            });
            // "now" breaks into the reply straight away but aborts a running tool, so it waits for "next" then
            if steer {
                line["priority"] = json!(if self.tool_running() { "next" } else { "now" });
            }
            self.claude_waiting.push_back(sent);
            self.claude_sent.push(line.clone());
            self.write(line);
        } else if self.settings.provider == Provider::OpenCode {
            if self.thread_live {
                self.opencode_send(text, attachments);
            } else {
                self.queued.push((text, attachments));
            }
        } else if let (true, true, Some(thread), Some(turn)) = (
            steer,
            self.thread_live,
            self.thread_id.clone(),
            self.turn_id.clone(),
        ) {
            // codex takes new input into the turn that's already running
            let mut params = turn_params(&thread, &text, &attachments, &self.settings);
            add_skills(&mut params, &text, &self.skills());
            params =
                json!({ "threadId": thread, "expectedTurnId": turn, "input": params["input"] });
            self.call(Call::Steer(text, attachments), "turn/steer", params);
        } else if self.thread_live {
            self.start_turn(text, attachments);
        } else {
            self.queued.push((text, attachments));
        }
    }

    // whether the current turn has a tool call in progress, leaving out ones running in the background
    fn tool_running(&self) -> bool {
        // the messages being sent now are already in the transcript, so the turn starts before them
        self.items
            .iter()
            .rev()
            .skip_while(|item| matches!(item, AgentItem::User { .. }))
            .take_while(|item| !matches!(item, AgentItem::User { .. }))
            .any(|item| match item {
                AgentItem::Command {
                    status, background, ..
                } => *status == ItemStatus::Running && !background,
                AgentItem::Subagent {
                    status, background, ..
                } => *status == ItemStatus::Running && !background,
                AgentItem::Tool { status, .. } | AgentItem::FileChange { status, .. } => {
                    *status == ItemStatus::Running
                }
                _ => false,
            })
    }

    // summarises the conversation so far, which only makes sense between turns of a started one
    pub fn compact(
        &mut self,
        session: u64,
        generation: u64,
        cwd: PathBuf,
        events: &UnboundedSender<Event>,
    ) {
        let claude = self.settings.provider == Provider::Claude;
        let opencode = self.settings.provider == Provider::OpenCode;
        let started = if claude || opencode {
            self.session_id.is_some()
        } else {
            self.thread_id.is_some()
        };
        if self.busy || !started {
            return;
        }
        let index = self.push(AgentItem::Compaction {
            status: ItemStatus::Running,
        });
        self.compacting = Some(index);
        self.set_busy(true);
        if !self.launch(false, session, generation, cwd, events) {
            self.finish_compaction(ItemStatus::Failed);
            return;
        }
        if claude {
            // claude never echoes /compact itself, so its turn counts as started now
            self.claude_turn = true;
            self.write(json!({
                "type": "user",
                "message": { "role": "user", "content": "/compact" }
            }));
        } else if opencode {
            self.opencode_compact();
        } else if self.thread_live {
            self.start_compaction();
        } else {
            self.compact_queued = true;
        }
    }

    fn start_compaction(&mut self) {
        if let Some(thread) = self.thread_id.clone() {
            self.call(
                Call::Compact,
                "thread/compact/start",
                json!({ "threadId": thread }),
            );
        }
    }

    fn finish_compaction(&mut self, done: ItemStatus) {
        if let Some(index) = self.compacting.take()
            && let Some(AgentItem::Compaction { status }) = self.items.get_mut(index)
        {
            *status = done;
            self.changed.insert(index);
        }
    }

    // starts the cli when it isn't running, reporting why when it can't be
    fn launch(
        &mut self,
        busy: bool,
        session: u64,
        generation: u64,
        cwd: PathBuf,
        events: &UnboundedSender<Event>,
    ) -> bool {
        let provider = self.settings.provider;
        // claude takes most new settings while it runs, and the rest mean relaunching with --resume,
        // which waits for a quiet moment rather than cutting off a turn in progress
        if provider == Provider::Claude
            && !busy
            && self.launched.as_ref() != Some(&self.settings)
            && !self.reconfigure_claude()
        {
            self.process = None;
        }
        if self.process.is_some() {
            return true;
        }
        let Err(err) = self.start(session, generation, cwd, events) else {
            return true;
        };
        let name = provider.label();
        let message = if err.kind() == io::ErrorKind::NotFound {
            format!("The {name} CLI wasn't found on PATH. Install it, then send again.")
        } else {
            format!("Couldn't start {name}: {err}")
        };
        self.push(AgentItem::Error { message });
        self.set_busy(false);
        false
    }

    // the agent is trying a failed request again, shown on one row that counts the attempts
    fn retrying(&mut self, reason: String, attempt: u32, of: Option<u32>) {
        let item = AgentItem::Retry {
            reason,
            attempt,
            of,
            status: ItemStatus::Running,
        };
        match self.live_retry {
            Some(index) if matches!(self.items.get(index), Some(AgentItem::Retry { .. })) => {
                self.items[index] = item;
                self.changed.insert(index);
            }
            _ => self.live_retry = Some(self.push(item)),
        }
    }

    // the turn got going again, or ended, so the retrying is over one way or the other
    fn retried(&mut self, outcome: ItemStatus) {
        if let Some(index) = self.live_retry.take()
            && let Some(AgentItem::Retry { status, .. }) = self.items.get_mut(index)
        {
            *status = outcome;
            self.changed.insert(index);
        }
    }

    // marks the oldest waiting messages as picked up, up to `count` of them
    fn unqueue(&mut self, count: usize) {
        let waiting: Vec<usize> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| matches!(item, AgentItem::User { queued: true, .. }))
            .map(|(i, _)| i)
            .take(count)
            .collect();
        for index in waiting {
            if let Some(AgentItem::User { queued, .. }) = self.items.get_mut(index) {
                *queued = false;
                self.changed.insert(index);
            }
        }
    }

    pub fn error(&mut self, message: String) {
        self.push(AgentItem::Error { message });
    }

    pub fn decide(&mut self, approval: u64, decision: Decision) {
        let Some(pending) = self.approvals.remove(&approval) else {
            return;
        };
        let index = pending.index;
        if self.settings.provider == Provider::Claude {
            let answer = claude_answer(decision, pending.input, &pending.suggestions);
            self.write(json!({
                "type": "control_response",
                "response": { "subtype": "success", "request_id": pending.request, "response": answer }
            }));
        } else if self.settings.provider == Provider::OpenCode {
            self.opencode_choose(&pending.request, &pending.input, decision);
        } else if let Some(persist) = pending.input.get("mcp") {
            // codex asks before an mcp tool runs as a form, where always allowing lasts the session
            let mut result = match decision {
                Decision::Deny => json!({ "action": "decline" }),
                _ => json!({ "action": "accept" }),
            };
            let lasting = persist
                .as_array()
                .is_some_and(|p| p.contains(&json!("session")));
            if decision == Decision::AlwaysAllow && lasting {
                result["_meta"] = json!({ "persist": "session" });
            }
            self.write(json!({ "id": pending.request, "result": result }));
        } else {
            let answer = match decision {
                Decision::Allow => "accept",
                Decision::AlwaysAllow => "acceptForSession",
                Decision::Deny => "decline",
            };
            self.write(json!({ "id": pending.request, "result": { "decision": answer } }));
        }
        if let Some(AgentItem::Approval { decision: d, .. }) = self.items.get_mut(index) {
            *d = Some(decision);
            self.changed.insert(index);
        }
    }

    // the queue when it changed since the last call, with whether it's held
    pub fn take_queue(&mut self) -> Option<(Vec<Queued>, bool)> {
        std::mem::take(&mut self.queue_changed).then(|| (self.queue.clone(), self.held))
    }

    // a tool call is where claude code slips waiting messages in, since the agent reads them with the tool's result,
    // so they go to the agent then rather than waiting for the turn to end
    fn deliver_queued(&mut self) {
        let steerable = match self.settings.provider {
            Provider::Claude => true,
            Provider::Codex => self.thread_live && self.turn_id.is_some(),
            // opencode can't take a message mid turn, so its queue waits for the end
            Provider::OpenCode => false,
        };
        if !steerable || self.held || !self.busy || self.queue.is_empty() || self.process.is_none()
        {
            return;
        }
        self.queue_changed = true;
        for message in std::mem::take(&mut self.queue) {
            self.record(&message.text, &message.attachments, true);
            self.transmit(message.text, message.attachments, true);
        }
    }

    // the next waiting message goes out once a turn ends, one turn each
    fn next_queued(&mut self) {
        if self.held || self.busy || self.queue.is_empty() || self.process.is_none() {
            return;
        }
        let next = self.queue.remove(0);
        self.queue_changed = true;
        self.record(&next.text, &next.attachments, false);
        self.transmit(next.text, next.attachments, false);
    }

    // edits, drops or hurries one waiting message, returning one to send through a fresh start when the agent is idle
    pub fn change_queue(&mut self, index: usize, action: QueueAction) -> Option<Queued> {
        if index >= self.queue.len() {
            return None;
        }
        self.queue_changed = true;
        match action {
            QueueAction::Edit(text) => self.queue[index].text = text,
            QueueAction::Remove => {
                self.queue.remove(index);
            }
            QueueAction::SendNow => {
                let message = self.queue.remove(index);
                self.held = false;
                if !self.busy || self.process.is_none() {
                    return Some(message);
                }
                if self.settings.provider == Provider::OpenCode {
                    // opencode can't take a message mid reply, so the reply stops and this one goes next
                    self.queue.insert(0, message);
                    self.stop();
                    self.held = false;
                } else {
                    // claude reads it at its next step and codex joins it to the running turn
                    self.record(&message.text, &message.attachments, true);
                    self.transmit(message.text, message.attachments, true);
                }
            }
        }
        None
    }

    // carries on with a held queue, returning the first message when there's no running agent to hand it to
    pub fn resume(&mut self) -> Option<Queued> {
        self.held = false;
        self.queue_changed = true;
        if self.process.is_none() && !self.busy && !self.queue.is_empty() {
            return Some(self.queue.remove(0));
        }
        self.next_queued();
        None
    }

    pub fn stop(&mut self) {
        // stopping holds anything waiting, so it doesn't start the moment the agent stops
        if !self.queue.is_empty() {
            self.held = true;
            self.queue_changed = true;
        }
        if self.settings.provider == Provider::Claude {
            if self.busy {
                self.write(json!({
                    "type": "control_request",
                    "request_id": format!("mux-stop-{}", self.next_call),
                    "request": { "subtype": "interrupt" }
                }));
                self.next_call += 1;
                self.claude_stopping = true;
            }
            return;
        }
        if !self.thread_live {
            self.queued.clear();
            self.compact_queued = false;
            self.unqueue(usize::MAX);
            self.finish_compaction(ItemStatus::Failed);
            self.set_busy(false);
            return;
        }
        if self.settings.provider == Provider::OpenCode {
            if self.pending_prompt.take().is_some() {
                self.finish_compaction(ItemStatus::Failed);
                self.set_busy(false);
                return;
            }
            self.opencode_stop();
            return;
        }
        if let (Some(thread), Some(turn)) = (self.thread_id.clone(), self.turn_id.clone()) {
            self.stop_requested = false;
            self.call(
                Call::Interrupt,
                "turn/interrupt",
                json!({ "threadId": thread, "turnId": turn }),
            );
        } else if self.busy
            && self
                .calls
                .values()
                .any(|call| matches!(call, Call::StartTurn))
        {
            self.stop_requested = true;
        }
    }

    pub fn exited(&mut self) {
        if std::mem::take(&mut self.claude_lost) && self.start_over() {
            return;
        }
        self.process = None;
        self.claude_stopping = false;
        if !self.queue.is_empty() {
            self.held = true;
            self.queue_changed = true;
        }
        self.thread_live = false;
        self.pending_prompt = None;
        self.stop_requested = false;
        self.calls.clear();
        if self.busy {
            self.push(AgentItem::Error {
                message: format!("{} stopped unexpectedly.", self.settings.provider.label()),
            });
        }
        self.retried(ItemStatus::Failed);
        self.expire_approvals();
        self.expire_questions();
        self.turn_id = None;
        self.live_text = None;
        self.launched = None;
        self.claude_waiting.clear();
        self.claude_turn = false;
        // whatever was waiting will never run on this process
        self.queued.clear();
        self.unqueue(usize::MAX);
        self.compact_queued = false;
        self.finish_compaction(ItemStatus::Failed);
        self.fail_subagents();
        self.stop_all_background();
        // nothing the old process was running can finish now, and its ids mean nothing to the next one
        for (index, item) in self.items.iter_mut().enumerate() {
            if let AgentItem::Command { status, .. }
            | AgentItem::Tool { status, .. }
            | AgentItem::FileChange { status, .. } = item
                && *status == ItemStatus::Running
            {
                *status = ItemStatus::Failed;
                self.changed.insert(index);
            }
        }
        self.by_codex_id.clear();
        self.set_busy(false);
    }

    pub fn handle(&mut self, msg: Value) {
        if self.settings.provider == Provider::Claude {
            self.handle_claude(&msg);
            return;
        }
        if self.settings.provider == Provider::OpenCode {
            self.handle_opencode(&msg);
            return;
        }
        let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
        match (method, msg.get("id").cloned()) {
            (Some(method), Some(id)) => self.request(&method, id, &msg["params"]),
            (Some(method), None) => self.notification(&method, &msg["params"]),
            (None, Some(id)) => self.response(id, &msg),
            (None, None) => {}
        }
    }

    fn fail(&mut self, message: String) {
        self.push(AgentItem::Error { message });
        self.set_busy(false);
    }

    // approvals that codex dropped can't be answered any more
    fn expire_approvals(&mut self) {
        for (_, Pending { index, .. }) in self.approvals.drain() {
            if let Some(AgentItem::Approval { decision, .. }) = self.items.get_mut(index) {
                *decision = Some(Decision::Deny);
                self.changed.insert(index);
            }
        }
    }

    fn call(&mut self, call: Call, method: &str, params: Value) {
        let id = self.next_call;
        self.next_call += 1;
        self.calls.insert(id, call);
        self.write(json!({ "id": id, "method": method, "params": params }));
    }

    fn write(&self, msg: Value) {
        if let Some(codex) = &self.process {
            let _ = codex.stdin.send(format!("{msg}\n"));
        }
    }

    fn push(&mut self, item: AgentItem) -> usize {
        // the model has moved on from thinking once it replies or acts
        if !matches!(item, AgentItem::Reasoning { .. }) {
            self.end_reasoning();
        }
        self.items.push(item);
        let index = self.items.len() - 1;
        self.changed.insert(index);
        index
    }

    // streamed reasoning joins the thought in progress, or starts one
    fn reason(&mut self, chunk: &str) -> usize {
        if let Some(index) = self.live_reasoning
            && matches!(self.items.get(index), Some(AgentItem::Reasoning { .. }))
        {
            self.append(index, chunk);
            return index;
        }
        let index = self.push(AgentItem::Reasoning {
            text: chunk.to_owned(),
            started: now_ms(),
            ended: None,
        });
        self.live_reasoning = Some(index);
        index
    }

    fn end_reasoning(&mut self) {
        if let Some(index) = self.live_reasoning.take()
            && let Some(AgentItem::Reasoning { ended, .. }) = self.items.get_mut(index)
        {
            *ended = Some(now_ms());
            self.changed.insert(index);
        }
    }

    fn set_busy(&mut self, busy: bool) {
        if !busy {
            self.stop_requested = false;
            self.end_reasoning();
        }
        if self.busy
            && !busy
            && let Some((index, at)) = self.turn_started.take()
            && let Some(AgentItem::User { took, .. }) = self.items.get_mut(index)
        {
            *took = Some(now_ms().saturating_sub(at));
            self.changed.insert(index);
        }
        if self.busy != busy {
            self.busy = busy;
            self.busy_changed = true;
        }
    }

    fn target_mut(&mut self, params: &Value) -> Option<&mut AgentItem> {
        let index = *self.by_codex_id.get(params["itemId"].as_str()?)?;
        self.changed.insert(index);
        self.items.get_mut(index)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

// attached files and folders as paths in the message, which an agent can read or list itself
fn attached(paths: &[&String]) -> String {
    if paths.is_empty() {
        return String::new();
    }
    let mut out = "\n\nAttached:".to_owned();
    for path in paths {
        let kind = if Path::new(path.as_str()).is_dir() {
            " (folder)"
        } else {
            ""
        };
        out.push_str(&format!("\n- {path}{kind}"));
    }
    out
}

fn media_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

// what a claude session leaves in the environment of everything started from it, like a mux opened from a
// claude code terminal, which would make each agent think it runs nested inside that session
const PARENT_CLAUDE: [&str; 12] = [
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_PID",
    "CLAUDE_EFFORT",
    "CLAUDE_AGENT_SDK_VERSION",
];

// the one field that says what a tool call is about, like its path, pattern or url
fn tool_detail(input: &Value) -> String {
    [
        "file_path",
        "path",
        "pattern",
        "url",
        "query",
        "description",
        "prompt",
    ]
    .iter()
    .find_map(|key| input[key].as_str())
    .unwrap_or_default()
    .lines()
    .next()
    .unwrap_or_default()
    .to_owned()
}

fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn item_status(item: &Value) -> ItemStatus {
    match item["status"].as_str() {
        Some("completed") => ItemStatus::Done,
        Some("declined") => ItemStatus::Declined,
        Some("inProgress") => ItemStatus::Running,
        _ => ItemStatus::Failed,
    }
}

fn trim_front(output: &mut String) {
    if output.len() > OUTPUT_LIMIT {
        let mut cut = output.len() - OUTPUT_LIMIT;
        while !output.is_char_boundary(cut) {
            cut += 1;
        }
        output.drain(..cut);
    }
}

#[cfg(test)]
mod tests;
