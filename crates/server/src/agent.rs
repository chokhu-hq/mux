use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine;
use mux_core::protocol::{
    AgentItem, AgentSettings, AgentSummary, ContextUsage, Decision, FileChange, ItemStatus,
    QueueAction, Queued, Skill, UsageWindow, is_image,
};
use mux_core::{Access, Provider, SessionStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::{Event, usage};

mod background;
mod group;
mod interaction;
mod opencode;
mod refresh;
mod rewind;
mod subagent;

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
    queue: Vec<Queued>,
    #[serde(default)]
    resumes: u32,
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
    Configure,
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
    // commands the agent left running in the background, and ones that finished since the last message
    background: Vec<background::Tracked>,
    background_changed: bool,
    // where background output is reported, kept from the launch for the followers it starts
    events: Option<UnboundedSender<Event>>,
    session: u64,
}

impl Agent {
    pub fn load(path: &Path) -> Self {
        let saved: Saved = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
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
            claude_turn: false,
            claude_stopping: false,
            sub_steps: HashMap::new(),
            tasks: HashMap::new(),
            child_threads: HashMap::new(),
            questions: HashMap::new(),
            todos: None,
            default_model: None,
            claude_head: saved.claude_head,
            resume_at: None,
            pending_revert: None,
            // nothing runs a saved queue until the person asks, since the agent it was meant for has gone
            held: !saved.queue.is_empty(),
            queue: saved.queue,
            resumes: saved.resumes,
            limit_reset: None,
            windows: Vec::new(),
            pause: None,
            saved_ids: ids,
            queue_changed: false,
            compacting: None,
            compact_queued: false,
            applied: opencode::Applied::default(),
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
            queue: self.queue.clone(),
            resumes: self.resumes,
        };
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&saved).map_err(io::Error::other)?)?;
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
    fn pause_until(&mut self, reset: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let wait = reset.saturating_sub(now);
        let when = match wait {
            0..=90 => "in a minute or so".to_owned(),
            91..=3599 => format!("in about {} min", wait.div_ceil(60)),
            _ => format!("in about {} h {} min", wait / 3600, (wait % 3600) / 60),
        };
        self.push(AgentItem::Error {
            message: format!(
                "Mux will carry on by itself {when}, once the limit resets. Sending a message takes over instead."
            ),
        });
        self.pause = Some(reset);
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

    // the item codex's notification is about, for adding streamed text to
    fn codex_index(&self, params: &Value) -> Option<usize> {
        self.by_codex_id.get(params["itemId"].as_str()?).copied()
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
        self.push(AgentItem::User {
            text: text.to_owned(),
            attachments: attachments.to_vec(),
            queued,
            rewind,
        });
        self.set_busy(true);
    }

    fn transmit(&mut self, text: String, attachments: Vec<String>, steer: bool) {
        if self.settings.provider == Provider::Claude {
            let mut line = json!({
                "type": "user",
                "message": { "role": "user", "content": claude_content(&text, &attachments, &self.skills()) }
            });
            // "now" breaks into the reply straight away but aborts a running tool, so it waits for "next" then
            if steer {
                line["priority"] = json!(if self.tool_running() { "next" } else { "now" });
            }
            self.claude_waiting.push_back(text);
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

    // starts claude on a new conversation after the old one was lost, sending again what the old one never read
    fn start_over(&mut self) -> bool {
        let (Some(events), Some(cwd)) = (self.events.clone(), self.cwd.clone()) else {
            return false;
        };
        let sent = std::mem::take(&mut self.claude_sent);
        self.process = None;
        self.session_id = None;
        self.claude_head = None;
        self.resume_at = None;
        self.push(AgentItem::Error {
            message: "Claude no longer had this conversation, so it started a new one. It won't remember what was said before.".to_owned(),
        });
        if let Err(err) = self.start(self.session, self.generation, cwd, &events) {
            log::warn!("couldn't start claude again: {err}");
            return false;
        }
        for line in sent {
            self.claude_sent.push(line.clone());
            self.write(line);
        }
        true
    }

    // hands new settings to the running claude, written ahead of the message they're for, or says it needs a relaunch
    fn reconfigure_claude(&mut self) -> bool {
        let (Some(live), Some(_)) = (self.launched.clone(), self.process.as_ref()) else {
            return false;
        };
        let next = self.settings.clone();
        // bypassing permissions needs a flag at launch, and a stricter level shouldn't inherit what a looser one allowed
        let strictness = |access: Access| match access {
            Access::Full => 0,
            Access::Auto => 1,
            Access::AutoEdits => 2,
            Access::Supervised => 3,
        };
        if live.provider != next.provider
            || live.access == Access::Full
            || next.access == Access::Full
            || strictness(next.access) > strictness(live.access)
        {
            return false;
        }
        if live.model != next.model {
            self.claude_control("set_model", json!({ "model": next.model }));
        }
        if live.effort != next.effort || live.fast != next.fast {
            // null hands the setting back to whatever the user has claude set to
            let settings =
                json!({ "effortLevel": next.effort, "fastMode": next.fast.then_some(true) });
            self.claude_control("apply_flag_settings", json!({ "settings": settings }));
        }
        if permission_mode(&live) != permission_mode(&next) {
            self.claude_control(
                "set_permission_mode",
                json!({ "mode": permission_mode(&next) }),
            );
        }
        log::info!(
            "session {}: changed claude's settings in place",
            self.session
        );
        self.launched = Some(next);
        true
    }

    fn claude_control(&mut self, subtype: &str, mut request: Value) {
        request["subtype"] = json!(subtype);
        self.write(json!({
            "type": "control_request",
            "request_id": format!("{CONFIG_REQUEST}{}", self.next_call),
            "request": request,
        }));
        self.next_call += 1;
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
        if self.settings.provider == Provider::OpenCode {
            self.opencode_stop();
            return;
        }
        if let (Some(thread), Some(turn)) = (self.thread_id.clone(), self.turn_id.clone()) {
            self.call(
                Call::Interrupt,
                "turn/interrupt",
                json!({ "threadId": thread, "turnId": turn }),
            );
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
        self.unqueue(usize::MAX);
        self.compact_queued = false;
        self.finish_compaction(ItemStatus::Failed);
        self.fail_subagents();
        self.stop_all_background();
        self.set_busy(false);
    }

    fn start(
        &mut self,
        session: u64,
        generation: u64,
        cwd: PathBuf,
        events: &UnboundedSender<Event>,
    ) -> io::Result<()> {
        let claude = self.settings.provider == Provider::Claude;
        let opencode = self.settings.provider == Provider::OpenCode;
        let mut command = if opencode {
            let mut command = Command::from(mux_core::command("opencode"));
            command.arg("acp");
            command
        } else if claude {
            let mut command = Command::from(mux_core::command("claude"));
            forget_parent_claude(&mut command);
            command.args(claude_args(
                &self.settings,
                self.session_id.as_deref(),
                self.resume_at.take().as_deref(),
            ));
            command
        } else {
            let mut command = Command::from(mux_core::command("codex"));
            command.arg("app-server");
            command
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .current_dir(&cwd);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn()?;
        let group = match group::watch(&child) {
            Ok(group) => Some(group),
            Err(err) => {
                log::warn!("session {session}: couldn't watch the agent's processes: {err}");
                None
            }
        };
        let provider = self.settings.provider.label();
        log::info!("session {session}: started {provider} in {}", cwd.display());
        // what the cli says on stderr is often the only clue when it fails, so the start of it is kept in the log
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut kept = 0;
                while let Ok(Some(line)) = lines.next_line().await {
                    if kept < STDERR_LINES {
                        log::info!(target: "mux::agent", "session {session}: {provider}: {line}");
                    } else if kept == STDERR_LINES {
                        log::info!(target: "mux::agent", "session {session}: {provider}: more output left out");
                    }
                    kept += 1;
                }
            });
        }
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("no stdin"))?;

        let (tx, mut outgoing) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = outgoing.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        self.events = Some(events.clone());
        let events = events.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                let event = Event::Agent {
                    session,
                    generation,
                    msg,
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::AgentExited {
                session,
                generation,
            });
        });

        self.generation = generation;
        self.session = session;
        self.process = Some(Process {
            stdin: tx,
            child: Some(child),
            claude,
            group,
        });
        self.cwd = Some(cwd);
        if claude {
            self.launched = Some(self.settings.clone());
            self.claude_sent.clear();
            // without this an interrupt stops claude's background agents along with the turn
            self.write(json!({
                "type": "control_request",
                "request_id": "mux-initialize",
                "request": { "subtype": "initialize", "perTaskStopAffordance": true }
            }));
            return Ok(());
        }
        if opencode {
            self.opencode_initialize();
            return Ok(());
        }
        self.call(
            Call::Initialize,
            "initialize",
            // plan mode is still one of codex's experimental fields
            json!({
                "clientInfo": { "name": "mux", "title": "Mux", "version": mux_core::VERSION },
                "capabilities": { "experimentalApi": true },
            }),
        );
        Ok(())
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

    fn response(&mut self, id: Value, msg: &Value) {
        let Some(call) = id.as_u64().and_then(|id| self.calls.remove(&id)) else {
            return;
        };
        let error = msg
            .get("error")
            .map(|e| e["message"].as_str().unwrap_or("unknown error").to_owned());
        match call {
            Call::Initialize => {
                self.write(json!({ "method": "initialized" }));
                self.open_thread();
            }
            Call::ResumeThread if error.is_some() => {
                // codex no longer has the old thread, so carry on in a fresh one
                self.thread_id = None;
                self.open_thread();
            }
            Call::StartThread | Call::ResumeThread => match error {
                Some(message) => self.fail(format!("Couldn't start a Codex thread: {message}")),
                None => {
                    self.thread_id = msg["result"]["thread"]["id"].as_str().map(str::to_owned);
                    self.thread_live = true;
                    if let Some(turn) = self.pending_revert.take() {
                        self.revert(turn);
                    }
                    if std::mem::take(&mut self.compact_queued) {
                        self.start_compaction();
                    }
                    let queued = std::mem::take(&mut self.queued);
                    self.unqueue(usize::MAX);
                    if !queued.is_empty() {
                        let (texts, files): (Vec<_>, Vec<_>) = queued.into_iter().unzip();
                        self.start_turn(texts.join("\n\n"), files.concat());
                    }
                }
            },
            Call::StartTurn => match error {
                Some(message) => self.fail(format!("Codex didn't start the turn: {message}")),
                None => self.turn_id = msg["result"]["turn"]["id"].as_str().map(str::to_owned),
            },
            Call::Steer(text, attachments) => {
                if error.is_some() {
                    self.start_turn(text, attachments);
                }
            }
            Call::Interrupt | Call::Configure => {}
            Call::Revert => {
                if let Some(message) = error {
                    self.push(AgentItem::Error {
                        message: format!("Codex couldn't wind back to that message: {message}"),
                    });
                }
            }
            Call::Compact => {
                if let Some(message) = error {
                    self.finish_compaction(ItemStatus::Failed);
                    self.fail(format!(
                        "Codex couldn't compact the conversation: {message}"
                    ));
                }
            }
        }
    }

    fn request(&mut self, method: &str, id: Value, params: &Value) {
        if method == "item/tool/requestUserInput" {
            self.codex_question(id, params);
            return;
        }
        let target = params["itemId"]
            .as_str()
            .and_then(|item| self.by_codex_id.get(item))
            .and_then(|&i| self.items.get(i));
        let (title, detail) = match method {
            "item/commandExecution/requestApproval" => {
                let command = match target {
                    Some(AgentItem::Command { command, .. }) => command.clone(),
                    _ => params["command"].as_str().unwrap_or_default().to_owned(),
                };
                ("Run a command?", command)
            }
            "item/fileChange/requestApproval" => {
                let paths = match target {
                    Some(AgentItem::FileChange { changes, .. }) => changes
                        .iter()
                        .map(|c| c.path.as_str())
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                ("Apply file changes?", paths)
            }
            _ => {
                log::warn!("codex asked for {method}, which mux doesn't support yet");
                // anything mux can't show yet is refused so codex never waits forever
                self.write(json!({
                    "id": id,
                    "error": { "code": -32601, "message": format!("{method} is not supported by mux") }
                }));
                return;
            }
        };
        self.next_approval += 1;
        let approval = self.next_approval;
        let index = self.push(AgentItem::Approval {
            id: approval,
            title: title.to_owned(),
            detail,
            decision: None,
        });
        self.approvals.insert(
            approval,
            Pending {
                request: id,
                index,
                input: Value::Null,
                suggestions: Value::Null,
            },
        );
    }

    fn notification(&mut self, method: &str, params: &Value) {
        if self.codex_child(method, params) {
            return;
        }
        match method {
            "turn/started" => {
                self.turn_id = params["turn"]["id"].as_str().map(str::to_owned);
                if let Some(turn) = self.turn_id.clone() {
                    self.mark_turn(&turn);
                }
                self.set_busy(true);
            }
            "turn/completed" => {
                let turn = &params["turn"];
                self.retried(if turn["status"] == "failed" {
                    ItemStatus::Failed
                } else {
                    ItemStatus::Done
                });
                if turn["status"] == "failed" {
                    let message = turn["error"]["message"]
                        .as_str()
                        .unwrap_or("The turn failed.");
                    self.push(AgentItem::Error {
                        message: message.to_owned(),
                    });
                    if turn["error"]["codexErrorInfo"] == "usageLimitExceeded"
                        && let Some(reset) = usage::blocked_until(&self.windows)
                    {
                        self.pause_until(reset);
                    }
                }
                self.turn_id = None;
                self.expire_approvals();
                self.expire_questions();
                self.finish_compaction(ItemStatus::Failed);
                self.codex_backgrounded();
                self.set_busy(false);
                self.next_queued();
            }
            "error" if params["willRetry"] == true => {
                let (reason, attempt, of) =
                    codex_retry(params["error"]["message"].as_str().unwrap_or_default());
                self.retrying(reason, attempt, of);
            }
            "error" => {
                let message = params["error"]["message"]
                    .as_str()
                    .unwrap_or("Codex reported an error.");
                self.push(AgentItem::Error {
                    message: message.to_owned(),
                });
            }
            // the last request's tokens are what the conversation currently fills
            "thread/tokenUsage/updated" => {
                let usage = &params["tokenUsage"];
                let window = usage["modelContextWindow"]
                    .as_u64()
                    .or(self.context.map(|c| c.window))
                    .unwrap_or(0);
                if let Some(used) = usage["last"]["totalTokens"].as_u64() {
                    self.set_context(ContextUsage { used, window });
                }
            }
            "turn/plan/updated" => self.codex_todos(params),
            "account/rateLimits/updated" => {
                self.windows = usage::codex(&params["rateLimits"]);
                self.usage = Some(self.windows.clone());
            }
            "item/started" => {
                self.retried(ItemStatus::Done);
                self.item_started(&params["item"]);
            }
            "item/completed" => self.item_completed(&params["item"]),
            "item/reasoning/summaryTextDelta" | "item/agentMessage/delta" => {
                self.retried(ItemStatus::Done);
                if let Some(index) = self.codex_index(params) {
                    self.append(index, params["delta"].as_str().unwrap_or_default());
                }
            }
            // each new part of a summary is its own paragraph
            "item/reasoning/summaryPartAdded" if params["summaryIndex"].as_u64() > Some(0) => {
                if let Some(index) = self.codex_index(params) {
                    self.append(index, "\n\n");
                }
            }
            "item/commandExecution/outputDelta" => {
                if let Some(AgentItem::Command { output, .. }) = self.target_mut(params) {
                    output.push_str(params["delta"].as_str().unwrap_or_default());
                    trim_front(output);
                }
                if let Some(&index) = params["itemId"]
                    .as_str()
                    .and_then(|id| self.by_codex_id.get(id))
                {
                    self.codex_background_item(index);
                }
            }
            _ => {}
        }
    }

    fn item_started(&mut self, item: &Value) {
        let Some(id) = item["id"].as_str().map(str::to_owned) else {
            return;
        };
        let entry = match item["type"].as_str() {
            Some("agentMessage") => AgentItem::Assistant {
                text: item["text"].as_str().unwrap_or_default().to_owned(),
            },
            Some("commandExecution") => AgentItem::Command {
                command: item["command"].as_str().unwrap_or_default().to_owned(),
                output: String::new(),
                exit_code: None,
                status: ItemStatus::Running,
                background: false,
            },
            Some("fileChange") => AgentItem::FileChange {
                changes: file_changes(item),
                status: ItemStatus::Running,
            },
            Some("collabAgentToolCall") => {
                self.codex_collab(item);
                return;
            }
            Some("plan") => AgentItem::Plan {
                text: item["text"].as_str().unwrap_or_default().to_owned(),
            },
            Some("reasoning") => {
                let index = self.reason("");
                self.by_codex_id.insert(id, index);
                return;
            }
            // a compaction mux asked for already has its item, and one codex started itself gets one
            Some("contextCompaction") => match self.compacting {
                Some(index) => {
                    self.by_codex_id.insert(id, index);
                    return;
                }
                None => AgentItem::Compaction {
                    status: ItemStatus::Running,
                },
            },
            _ => return,
        };
        let index = self.push(entry);
        if matches!(self.items[index], AgentItem::Compaction { .. }) {
            self.compacting = Some(index);
        }
        self.by_codex_id.insert(id, index);
        // codex takes waiting messages into the turn at a tool call, the same point claude does
        if matches!(
            self.items[index],
            AgentItem::Command { .. } | AgentItem::FileChange { .. }
        ) {
            self.deliver_queued();
        }
    }

    fn item_completed(&mut self, item: &Value) {
        if item["type"] == "collabAgentToolCall" {
            self.codex_collab(item);
            return;
        }
        let Some(&index) = item["id"].as_str().and_then(|id| self.by_codex_id.get(id)) else {
            return;
        };
        let status = item_status(item);
        match self.items.get_mut(index) {
            Some(AgentItem::Assistant { text }) => {
                *text = item["text"].as_str().unwrap_or_default().to_owned();
            }
            Some(AgentItem::Command {
                output,
                exit_code,
                status: s,
                ..
            }) => {
                if let Some(full) = item["aggregatedOutput"].as_str() {
                    *output = full.to_owned();
                    trim_front(output);
                }
                *exit_code = item["exitCode"].as_i64().map(|c| c as i32);
                *s = match (status, *exit_code) {
                    (ItemStatus::Done, Some(code)) if code != 0 => ItemStatus::Failed,
                    (status, _) => status,
                };
            }
            Some(AgentItem::FileChange { changes, status: s }) => {
                *changes = file_changes(item);
                *s = status;
            }
            Some(AgentItem::Plan { text }) => {
                *text = item["text"].as_str().unwrap_or_default().to_owned();
            }
            // the finished summary is authoritative over what streamed in
            Some(AgentItem::Reasoning { text, .. }) => {
                let summary: Vec<&str> = item["summary"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if !summary.is_empty() {
                    *text = summary.join("\n\n");
                }
                if self.live_reasoning == Some(index) {
                    self.end_reasoning();
                }
            }
            // the compaction item has no status, so finishing it is what says it worked
            Some(AgentItem::Compaction { status: s }) => {
                *s = ItemStatus::Done;
                self.compacting = None;
            }
            _ => return,
        }
        self.changed.insert(index);
        self.codex_background_item(index);
    }

    fn handle_claude(&mut self, msg: &Value) {
        // a subagent's own messages carry the id of the tool call that started it
        if let Some(parent) = msg["parent_tool_use_id"].as_str() {
            self.claude_subagent_message(parent, msg);
            return;
        }
        match msg["type"].as_str() {
            Some("system")
                if msg["subtype"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("task_")) =>
            {
                if !self.claude_background_task(msg) {
                    self.claude_task(msg)
                }
            }
            // a single quick retry is nothing to show, so the row waits for the second
            Some("system") if msg["subtype"] == "api_retry" => {
                let attempt = msg["attempt"].as_u64().unwrap_or(1) as u32;
                if attempt >= 2 || self.live_retry.is_some() {
                    let of = msg["max_retries"].as_u64().map(|m| m as u32);
                    self.retrying(claude_retry_reason(msg), attempt, of);
                }
            }
            Some("system") if msg["subtype"] == "init" => {
                self.session_id = msg["session_id"].as_str().map(str::to_owned);
                // skills are prompts and work when claude runs headless, unlike its terminal commands
                let commands: Vec<String> = msg["skills"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|s| s.as_str().map(str::to_owned))
                    .collect();
                if !commands.is_empty() && commands != self.commands {
                    self.commands = commands;
                    self.commands_changed = true;
                }
            }
            // claude compacts on its own as the window fills, and says so before it starts
            Some("system") if msg["subtype"] == "status" && msg["status"] == "compacting" => {
                if self.compacting.is_none() {
                    let index = self.push(AgentItem::Compaction {
                        status: ItemStatus::Running,
                    });
                    self.compacting = Some(index);
                }
            }
            Some("system") if msg["subtype"] == "status" && msg["compact_result"].is_string() => {
                if msg["compact_result"] != "success" {
                    self.finish_compaction(ItemStatus::Failed);
                }
            }
            Some("system") if msg["subtype"] == "compact_boundary" => {
                if self.compacting.is_none() {
                    self.compacting = Some(self.push(AgentItem::Compaction {
                        status: ItemStatus::Running,
                    }));
                }
                self.finish_compaction(ItemStatus::Done);
                if let Some(used) = msg["compact_metadata"]["post_tokens"].as_u64() {
                    let window = self.context.map_or(0, |c| c.window);
                    self.set_context(ContextUsage { used, window });
                }
            }
            Some("stream_event") => {
                self.claude_turn = true;
                self.retried(ItemStatus::Done);
                let event = &msg["event"];
                match event["type"].as_str() {
                    Some("content_block_start") if event["content_block"]["type"] == "text" => {
                        let index = self.push(AgentItem::Assistant {
                            text: String::new(),
                        });
                        self.live_text = Some(index);
                    }
                    Some("content_block_start") if event["content_block"]["type"] == "thinking" => {
                        self.reason("");
                    }
                    Some("content_block_delta") if event["delta"]["type"] == "thinking_delta" => {
                        self.reason(event["delta"]["thinking"].as_str().unwrap_or_default());
                    }
                    Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                        if let Some(index) = self.live_text {
                            self.append(index, event["delta"]["text"].as_str().unwrap_or_default());
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                self.claude_turn = true;
                self.retried(ItemStatus::Done);
                // the latest reply is where a message sent next would be rewound to
                if let Some(uuid) = msg["uuid"].as_str() {
                    self.claude_head = Some(uuid.to_owned());
                }
                if let Some(used) = claude_context(&msg["message"]["usage"]) {
                    let window = self.context.map_or(0, |c| c.window);
                    self.set_context(ContextUsage { used, window });
                }
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    match block["type"].as_str() {
                        Some("text") => {
                            let text = block["text"].as_str().unwrap_or_default().to_owned();
                            // the finished block is authoritative over what streamed in
                            match self.live_text.take() {
                                Some(index) => {
                                    self.items[index] = AgentItem::Assistant { text };
                                    self.changed.insert(index);
                                }
                                None => {
                                    self.push(AgentItem::Assistant { text });
                                }
                            }
                        }
                        Some("tool_use") => {
                            self.claude_tool(block);
                            self.deliver_queued();
                        }
                        // the whole thought arrives once it's done, and fills in a stream that carried no words
                        Some("thinking") => {
                            let full = block["thinking"].as_str().unwrap_or_default();
                            let last = self
                                .items
                                .iter()
                                .rposition(|i| matches!(i, AgentItem::Reasoning { .. }));
                            if !full.is_empty()
                                && let Some(index) = last
                                && let Some(AgentItem::Reasoning { text, .. }) =
                                    self.items.get_mut(index)
                                && text.is_empty()
                            {
                                *text = full.to_owned();
                                self.changed.insert(index);
                            }
                        }
                        _ => {}
                    }
                }
            }
            Some("user") if msg["isReplay"] == true => self.claude_echo(msg),
            Some("user") => {
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        self.claude_tool_result(msg, block);
                    }
                }
            }
            Some("control_request") => self.claude_request(msg),
            // a setting claude wouldn't take in place is applied by relaunching at the next quiet moment instead
            Some("control_response")
                if msg["response"]["subtype"] == "error"
                    && msg["response"]["request_id"]
                        .as_str()
                        .is_some_and(|id| id.starts_with(CONFIG_REQUEST)) =>
            {
                log::warn!("claude refused a setting, so it'll be relaunched: {msg}");
                self.launched = None;
            }
            Some("rate_limit_event") => {
                let info = &msg["rate_limit_info"];
                self.usage = Some(usage::claude(info));
                self.limit_reset = usage::claude_blocked_until(info);
            }
            // a resumed claude that can't find its conversation says so straight away, then exits
            Some("result") if self.session_id.is_some() && !self.claude_turn && lost(msg) => {
                log::warn!("claude no longer has its conversation: {msg}");
                self.claude_lost = true;
            }
            Some("result") => {
                // only the result says how big the model's window is
                let window = msg["modelUsage"]
                    .as_object()
                    .into_iter()
                    .flat_map(|models| models.values())
                    .filter_map(|m| m["contextWindow"].as_u64())
                    .max();
                if let (Some(window), Some(context)) = (window, self.context) {
                    self.set_context(ContextUsage { window, ..context });
                }
                let failure = claude_failure(msg, self.claude_stopping);
                self.retried(if failure.is_some() {
                    ItemStatus::Failed
                } else {
                    ItemStatus::Done
                });
                if let Some(message) = failure {
                    log::warn!("claude turn failed: {msg}");
                    self.push(AgentItem::Error { message });
                    if let Some(reset) = self.limit_reset.take() {
                        self.pause_until(reset);
                    }
                }
                self.claude_stopping = false;
                self.live_text = None;
                self.expire_approvals();
                self.expire_questions();
                self.finish_compaction(ItemStatus::Failed);
                self.claude_turn = false;
                // messages it hasn't echoed yet will start a turn of their own
                self.set_busy(!self.claude_waiting.is_empty());
                self.next_queued();
            }
            _ => {}
        }
    }

    // claude echoes a message when it starts on it, and one echo can carry several sent close together
    fn claude_echo(&mut self, msg: &Value) {
        let content = &msg["message"]["content"];
        let text = match content {
            Value::String(text) => text.clone(),
            Value::Array(blocks) => blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            _ => return,
        };
        // the output of claude's own local commands comes back the same way and isn't a message
        if text.trim_start().starts_with("<local-command") {
            return;
        }
        if self.claude_waiting.pop_front().is_none() {
            return;
        }
        // it's reading the conversation, so it was there and nothing needs sending again
        self.claude_sent.clear();
        while let Some(next) = self.claude_waiting.front() {
            if next.trim().is_empty() || !text.contains(next.trim()) {
                break;
            }
            self.claude_waiting.pop_front();
        }
        self.claude_turn = true;
        self.set_busy(true);
        // whatever's still waiting stays queued, everything echoed has been picked up
        let queued = self
            .items
            .iter()
            .filter(|i| matches!(i, AgentItem::User { queued: true, .. }))
            .count();
        self.unqueue(queued.saturating_sub(self.claude_waiting.len()));
    }

    fn claude_tool(&mut self, block: &Value) {
        let Some(id) = block["id"].as_str().map(str::to_owned) else {
            return;
        };
        let input = &block["input"];
        let field = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
        let name = block["name"].as_str().unwrap_or_default();
        if name == "Agent" || name == "Task" {
            self.claude_subagent(id, input);
            return;
        }
        // the checklist is shown as one card that updates, not as a tool call each time
        if name == "TodoWrite" {
            self.claude_todos(input);
            return;
        }
        let entry = match name {
            "Bash" => AgentItem::Command {
                command: field("command"),
                output: String::new(),
                exit_code: None,
                status: ItemStatus::Running,
                background: input["run_in_background"] == true,
            },
            "Edit" | "MultiEdit" | "Write" => AgentItem::FileChange {
                changes: vec![FileChange {
                    path: field("file_path"),
                    diff: edit_diff(name, input),
                }],
                status: ItemStatus::Running,
            },
            _ => AgentItem::Tool {
                name: name.to_owned(),
                detail: tool_detail(input),
                output: String::new(),
                status: ItemStatus::Running,
            },
        };
        let index = self.push(entry);
        self.by_codex_id.insert(id, index);
    }

    fn claude_tool_result(&mut self, msg: &Value, block: &Value) {
        let Some(&index) = block["tool_use_id"]
            .as_str()
            .and_then(|id| self.by_codex_id.get(id))
        else {
            return;
        };
        if matches!(self.items.get(index), Some(AgentItem::Subagent { .. })) {
            self.claude_subagent_result(index, msg, block);
            return;
        }
        let failed = block["is_error"] == true;
        let status = if failed {
            ItemStatus::Failed
        } else {
            ItemStatus::Done
        };
        let text = result_text(&block["content"]);
        // a command moved to the background answers at once, but keeps running and its row with it
        if !failed
            && matches!(
                self.items.get(index),
                Some(AgentItem::Command {
                    background: true,
                    ..
                })
            )
            && self.claude_backgrounded(index, &text)
        {
            return;
        }
        match self.items.get_mut(index) {
            Some(AgentItem::Command {
                output, status: s, ..
            })
            | Some(AgentItem::Tool {
                output, status: s, ..
            }) => {
                *output = text;
                trim_front(output);
                *s = status;
            }
            Some(AgentItem::FileChange { status: s, .. }) => *s = status,
            _ => return,
        }
        self.changed.insert(index);
    }

    fn claude_request(&mut self, msg: &Value) {
        let request = &msg["request"];
        let id = msg["request_id"].clone();
        if request["subtype"] != "can_use_tool" {
            log::warn!(
                "claude sent a {} request, which mux doesn't support yet",
                request["subtype"]
            );
            // anything mux can't show yet is refused so claude never waits forever
            self.write(json!({
                "type": "control_response",
                "response": { "subtype": "error", "request_id": id, "error": "not supported by mux" }
            }));
            return;
        }
        let tool = request["tool_name"].as_str().unwrap_or_default();
        let input = request["input"].clone();
        match tool {
            "AskUserQuestion" => return self.claude_question(id, input),
            "ExitPlanMode" => return self.claude_plan(id, &input),
            _ => {}
        }
        let (title, detail) = match tool {
            "Bash" => (
                "Run a command?".to_owned(),
                input["command"].as_str().unwrap_or_default().to_owned(),
            ),
            "Edit" | "MultiEdit" | "Write" => (
                "Apply file changes?".to_owned(),
                input["file_path"].as_str().unwrap_or_default().to_owned(),
            ),
            other => (format!("Allow {other}?"), tool_detail(&input)),
        };
        self.next_approval += 1;
        let approval = self.next_approval;
        let index = self.push(AgentItem::Approval {
            id: approval,
            title,
            detail,
            decision: None,
        });
        self.approvals.insert(
            approval,
            Pending {
                request: id,
                index,
                input,
                suggestions: request["permission_suggestions"].clone(),
            },
        );
    }

    fn open_thread(&mut self) {
        let mut params = json!({ "approvalPolicy": "on-request", "sandbox": "workspace-write" });
        if let Some(cwd) = &self.cwd {
            params["cwd"] = json!(cwd);
        }
        match self.thread_id.clone() {
            Some(thread) => {
                params["threadId"] = json!(thread);
                self.call(Call::ResumeThread, "thread/resume", params);
            }
            None => self.call(Call::StartThread, "thread/start", params),
        }
    }

    fn start_turn(&mut self, text: String, attachments: Vec<String>) {
        let Some(thread) = self.thread_id.clone() else {
            return;
        };
        let mut params = turn_params(&thread, &text, &attachments, &self.settings);
        add_skills(&mut params, &text, &self.skills());
        // both modes are always named, so turning plan mode off reaches codex too
        if let Some(model) = self.settings.model.clone().or(self.default_model.clone()) {
            let mode = if self.settings.plan {
                "plan"
            } else {
                "default"
            };
            params["collaborationMode"] = json!({ "mode": mode, "settings": { "model": model } });
        }
        self.call(Call::StartTurn, "turn/start", params);
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
            self.end_reasoning();
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

// sent on every turn, so a change in the picker applies to the very next message
// claude gets images inline, and other files as paths its read tool can open
fn claude_content(text: &str, attachments: &[String], skills: &[Skill]) -> Value {
    let (images, files): (Vec<&String>, Vec<&String>) =
        attachments.iter().partition(|p| is_image(p));
    // a mentioned skill needs its own block to start with `/name`, and the rest goes ahead of it
    let mut blocks = crate::skills::claude_blocks(text, skills);
    let mut text = blocks.pop().unwrap_or_default();
    if !files.is_empty() {
        text.push_str("\n\nAttached files:");
        for path in files {
            text.push_str(&format!("\n- {path}"));
        }
    }
    let mut content = Vec::new();
    for path in images {
        match fs::read(path) {
            // claude turns away images over 5 MB, which would fail the whole message
            Ok(bytes) if bytes.len() > 5 * 1024 * 1024 => {
                text.push_str(&format!("\n\n(Skipped {path} because it is over 5 MB)"))
            }
            Ok(bytes) => content.push(json!({ "type": "image", "source": {
                "type": "base64",
                "media_type": media_type(path),
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            } })),
            Err(err) => text.push_str(&format!("\n\n(Couldn't attach {path}: {err})")),
        }
    }
    for block in blocks {
        content.push(json!({ "type": "text", "text": block }));
    }
    content.push(json!({ "type": "text", "text": text }));
    Value::Array(content)
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

// everything claude sent with a request counts toward the window, cached or not
fn claude_context(usage: &Value) -> Option<u64> {
    let input = usage["input_tokens"].as_u64()?;
    Some(
        input
            + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + usage["output_tokens"].as_u64().unwrap_or(0),
    )
}

// codex reads images itself and gets other files as mentions it can open
// codex is handed each skill a message mentions by its path, alongside the text that names it
fn add_skills(params: &mut Value, text: &str, skills: &[Skill]) {
    let mut seen = Vec::new();
    for (_, _, skill) in crate::skills::mentions(text, skills) {
        if seen.contains(&&skill.name) || skill.path.is_empty() {
            continue;
        }
        seen.push(&skill.name);
        if let Some(input) = params["input"].as_array_mut() {
            input.push(json!({ "type": "skill", "name": skill.name, "path": skill.path }));
        }
    }
}

fn turn_params(
    thread: &str,
    text: &str,
    attachments: &[String],
    settings: &AgentSettings,
) -> Value {
    let mut input = vec![json!({ "type": "text", "text": text })];
    for path in attachments {
        input.push(if is_image(path) {
            json!({ "type": "localImage", "path": path })
        } else {
            json!({ "type": "mention", "name": file_name(path), "path": path })
        });
    }
    let mut params = json!({ "threadId": thread, "input": input });
    if let Some(model) = &settings.model {
        params["model"] = json!(model);
    }
    if let Some(effort) = &settings.effort {
        params["effort"] = json!(effort);
    }
    // "priority" is codex's fast tier, and "default" turns it back off explicitly
    params["serviceTierForTurn"] = json!(if settings.fast { "priority" } else { "default" });
    // codex leaves its reasoning out unless a summary of it is asked for
    params["summary"] = json!("auto");
    // these override the thread's defaults for this turn and the ones after it, so a switch applies right away
    let (approval, reviewer, sandbox) = match settings.access {
        Access::Supervised => ("untrusted", "user", "readOnly"),
        Access::AutoEdits => ("on-request", "user", "workspaceWrite"),
        Access::Auto => ("on-request", "auto_review", "workspaceWrite"),
        Access::Full => ("never", "user", "dangerFullAccess"),
    };
    params["approvalPolicy"] = json!(approval);
    params["approvalsReviewer"] = json!(reviewer);
    params["sandboxPolicy"] = json!({ "type": sandbox });
    params
}

fn claude_args(settings: &AgentSettings, session: Option<&str>, at: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // echoes each message as claude starts on it, which is how mux knows what's still waiting
        "--replay-user-messages",
        // sends permission prompts to us over stdio instead of answering them inside the cli
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        permission_mode(settings),
    ]
    .map(str::to_owned)
    .into();
    // claude refuses to bypass permissions unless it is explicitly allowed to
    if settings.access == Access::Full && !settings.plan {
        args.push("--allow-dangerously-skip-permissions".to_owned());
    }
    if let Some(model) = &settings.model {
        args.extend(["--model".to_owned(), model.clone()]);
    }
    if let Some(effort) = &settings.effort {
        args.extend(["--effort".to_owned(), effort.clone()]);
    }
    if settings.fast {
        args.extend([
            "--settings".to_owned(),
            json!({ "fastMode": true }).to_string(),
        ]);
    }
    if let Some(session) = session {
        args.extend(["--resume".to_owned(), session.to_owned()]);
        // a rewound conversation carries on from the reply before the edited message
        if let Some(at) = at {
            args.extend(["--resume-session-at".to_owned(), at.to_owned()]);
        }
    }
    args
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

pub(crate) fn forget_parent_claude(command: &mut Command) {
    for key in PARENT_CLAUDE {
        command.env_remove(key);
    }
}

// plan mode stands in for the access level until the plan is accepted
fn permission_mode(settings: &AgentSettings) -> &'static str {
    match settings.access {
        _ if settings.plan => "plan",
        Access::Supervised => "manual",
        Access::AutoEdits => "acceptEdits",
        Access::Auto => "auto",
        Access::Full => "bypassPermissions",
    }
}

// what's wrong in a few words, from the status and kind claude gives a retry
fn claude_retry_reason(msg: &Value) -> String {
    match (msg["error_status"].as_u64(), msg["error"].as_str()) {
        (Some(429), _) | (_, Some("rate_limit")) => "Claude is rate limited".to_owned(),
        (Some(529), _) | (_, Some("overloaded")) => "Claude's servers are busy".to_owned(),
        (Some(status), _) => format!("Claude returned an error ({status})"),
        (None, _) => "Couldn't reach Claude".to_owned(),
    }
}

// codex says "Reconnecting... 2/5 (stream disconnected)", with the attempt and why
fn codex_retry(message: &str) -> (String, u32, Option<u32>) {
    let counted = message.split_whitespace().find_map(|word| {
        let (n, of) = word.split_once('/')?;
        Some((n.parse().ok()?, of.parse().ok()?))
    });
    let why = message
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(why, _)| why.trim())
        .filter(|why| !why.is_empty());
    let reason = match why {
        Some(why) => format!("Codex lost its connection ({why})"),
        None => "Codex lost its connection".to_owned(),
    };
    match counted {
        Some((attempt, of)) => (reason, attempt, Some(of)),
        None => (reason, 1, None),
    }
}

// whether a result says claude couldn't find the conversation it was told to resume
fn lost(msg: &Value) -> bool {
    msg["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|e| e.starts_with("No conversation found"))
}

// why a turn failed in words, or none when it didn't or when it only ended because it was stopped
fn claude_failure(msg: &Value, stopping: bool) -> Option<String> {
    if msg["is_error"] != true && msg["subtype"] == "success" {
        return None;
    }
    // a stop ends the turn with an error result, but the stop is what was asked for
    let aborted = msg["terminal_reason"]
        .as_str()
        .is_some_and(|r| r.starts_with("aborted"));
    if stopping || aborted {
        return None;
    }
    if let Some(result) = msg["result"].as_str().filter(|r| !r.trim().is_empty()) {
        return Some(result.trim().to_owned());
    }
    let said = match msg["subtype"].as_str() {
        Some("error_max_turns") => {
            Some("Claude reached its turn limit before finishing.".to_owned())
        }
        Some("error_max_budget_usd") => {
            Some("Claude reached its spending limit before finishing.".to_owned())
        }
        _ => None,
    };
    // claude adds its own diagnostic lines to the errors, which mean nothing to a person
    let errors: Vec<&str> = msg["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|e| !e.starts_with('[') && !e.trim().is_empty())
        .collect();
    Some(said.unwrap_or_else(|| match errors.as_slice() {
        [] => "Claude stopped with an error. The server log has what it sent.".to_owned(),
        errors => format!("Claude stopped with an error: {}", errors.join(" ")),
    }))
}

// always allow keeps claude's own suggestions but only for this session, so nothing lands in project settings
fn claude_answer(decision: Decision, input: Value, suggestions: &Value) -> Value {
    match decision {
        Decision::Allow => json!({ "behavior": "allow", "updatedInput": input }),
        Decision::AlwaysAllow => {
            let session_only: Vec<Value> = suggestions
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    let mut s = s.clone();
                    s["destination"] = json!("session");
                    s
                })
                .collect();
            json!({ "behavior": "allow", "updatedInput": input, "updatedPermissions": session_only })
        }
        Decision::Deny => {
            json!({ "behavior": "deny", "message": "The user declined this in Mux." })
        }
    }
}

// shows claude's edits the way codex reports them, as a patch the diff card can read
fn edit_diff(tool: &str, input: &Value) -> String {
    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
    let hunk = |old: &str, new: &str| {
        let mut out = String::from("@@\n");
        for line in old.lines() {
            out.push_str(&format!("-{line}\n"));
        }
        for line in new.lines() {
            out.push_str(&format!("+{line}\n"));
        }
        out
    };
    match tool {
        "Write" => text(&input["content"]),
        "MultiEdit" => input["edits"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|e| hunk(&text(&e["old_string"]), &text(&e["new_string"])))
            .collect(),
        _ => hunk(&text(&input["old_string"]), &text(&input["new_string"])),
    }
}

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

fn file_changes(item: &Value) -> Vec<FileChange> {
    item["changes"]
        .as_array()
        .map(|changes| {
            changes
                .iter()
                .map(|c| FileChange {
                    path: c["path"].as_str().unwrap_or_default().to_owned(),
                    diff: c["diff"].as_str().unwrap_or_default().to_owned(),
                })
                .collect()
        })
        .unwrap_or_default()
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
mod tests {
    use super::*;
    use mux_core::protocol::TodoStatus;

    fn agent() -> Agent {
        Agent::load(Path::new("/nonexistent/mux-agent.json"))
    }

    #[test]
    fn a_stopped_claude_turn_is_not_an_error() {
        let aborted = json!({
            "type": "result",
            "subtype": "error_during_execution",
            "is_error": true,
            "result": null,
            "errors": ["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"],
            "terminal_reason": "aborted_tools",
        });
        assert_eq!(claude_failure(&aborted, false), None);
        let mut unexplained = aborted.clone();
        unexplained["terminal_reason"] = json!("completed");
        assert_eq!(claude_failure(&unexplained, true), None);
        assert!(
            claude_failure(&unexplained, false)
                .unwrap()
                .contains("server log")
        );
        let login = json!({
            "subtype": "success",
            "is_error": true,
            "result": "Invalid API key · Please run /login",
        });
        assert_eq!(
            claude_failure(&login, false).as_deref(),
            Some("Invalid API key · Please run /login")
        );
        let turns = json!({ "subtype": "error_max_turns", "is_error": true });
        assert!(
            claude_failure(&turns, false)
                .unwrap()
                .contains("turn limit")
        );
        let fine = json!({ "subtype": "success", "is_error": false, "result": "done" });
        assert_eq!(claude_failure(&fine, false), None);
    }

    fn note(agent: &mut Agent, method: &str, params: Value) {
        agent.handle(json!({ "method": method, "params": params }));
    }

    // codex 0.x with a summary asked for: a reasoning item, its streamed summary, then the finished one
    #[test]
    fn codex_reasoning_streams_then_takes_the_finished_summary() {
        let mut agent = agent();
        let item = json!({ "type": "reasoning", "id": "rs_1", "summary": [], "content": [] });
        note(&mut agent, "item/started", json!({ "item": item }));
        note(
            &mut agent,
            "item/reasoning/summaryTextDelta",
            json!({ "itemId": "rs_1", "delta": "**Counting**" }),
        );
        note(
            &mut agent,
            "item/reasoning/summaryPartAdded",
            json!({ "itemId": "rs_1", "summaryIndex": 1 }),
        );
        note(
            &mut agent,
            "item/reasoning/summaryTextDelta",
            json!({ "itemId": "rs_1", "delta": "Then check." }),
        );
        assert!(
            matches!(&agent.items[0], AgentItem::Reasoning { text, ended: None, .. } if text == "**Counting**\n\nThen check.")
        );
        let done = json!({ "type": "reasoning", "id": "rs_1", "summary": ["**Counting**", "Then check twice."], "content": [] });
        note(&mut agent, "item/completed", json!({ "item": done }));
        assert!(
            matches!(&agent.items[0], AgentItem::Reasoning { text, ended: Some(_), .. } if text == "**Counting**\n\nThen check twice.")
        );
    }

    // claude streams a thinking block, often with no words in it, then its reply
    #[test]
    fn claude_thinking_ends_when_the_reply_starts() {
        let mut agent = claude_agent();
        let event = |e: Value| json!({ "type": "stream_event", "event": e });
        agent.handle(event(json!({ "type": "content_block_start", "content_block": { "type": "thinking", "thinking": "" } })));
        agent.handle(event(json!({ "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": "" } })));
        assert!(matches!(
            &agent.items[0],
            AgentItem::Reasoning { ended: None, .. }
        ));
        agent.handle(event(
            json!({ "type": "content_block_start", "content_block": { "type": "text" } }),
        ));
        assert!(
            matches!(&agent.items[0], AgentItem::Reasoning { text, ended: Some(_), .. } if text.is_empty())
        );
        assert!(matches!(&agent.items[1], AgentItem::Assistant { .. }));
    }

    // the order claude 2.1.312 sent things in when two messages arrived during a turn and were answered together
    #[test]
    fn claude_is_done_when_merged_messages_get_one_answer() {
        let mut agent = claude_agent();
        let user = |text: &str| AgentItem::User {
            text: text.into(),
            attachments: Vec::new(),
            queued: false,
            rewind: None,
        };
        agent.items = vec![user("count")];
        agent.claude_waiting = VecDeque::from(["count".to_owned()]);
        agent.set_busy(true);
        let echo = |text: &str| json!({ "type": "user", "isReplay": true, "message": { "role": "user", "content": text } });
        let result = json!({ "type": "result", "subtype": "success", "result": "ok" });
        agent.handle(echo("count"));
        // two more arrive while it counts
        for text in ["Now say hi.", "And say bye."] {
            agent.items.push(AgentItem::User {
                text: text.into(),
                attachments: Vec::new(),
                queued: true,
                rewind: None,
            });
            agent.claude_waiting.push_back(text.into());
        }
        agent.handle(result.clone());
        assert!(agent.busy, "two messages are still waiting");
        agent.handle(echo("Now say hi.\nAnd say bye."));
        assert!(agent.claude_waiting.is_empty());
        assert!(
            !agent
                .items
                .iter()
                .any(|i| matches!(i, AgentItem::User { queued: true, .. }))
        );
        agent.handle(result.clone());
        assert!(!agent.busy);
        // a background subagent waking claude up later runs a turn nobody sent, and still ends cleanly
        agent.handle(json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "it finished" }] } }));
        agent.handle(result);
        assert!(!agent.busy);
    }

    #[test]
    fn claude_compaction_shows_in_the_transcript_and_shrinks_the_context() {
        let mut agent = claude_agent();
        agent.context = Some(ContextUsage {
            used: 150_000,
            window: 200_000,
        });
        agent.handle(json!({ "type": "system", "subtype": "status", "status": "compacting" }));
        assert_eq!(
            agent.items,
            [AgentItem::Compaction {
                status: ItemStatus::Running
            }]
        );
        agent.handle(json!({ "type": "system", "subtype": "compact_boundary",
            "compact_metadata": { "trigger": "auto", "pre_tokens": 150000, "post_tokens": 6000 } }));
        assert_eq!(
            agent.items,
            [AgentItem::Compaction {
                status: ItemStatus::Done
            }]
        );
        assert_eq!(agent.context.map(|c| c.used), Some(6000));
    }

    #[test]
    fn codex_compaction_reuses_the_item_mux_started() {
        let mut agent = agent();
        agent.compacting = Some(agent.push(AgentItem::Compaction {
            status: ItemStatus::Running,
        }));
        let item = json!({ "item": { "id": "k1", "type": "contextCompaction" } });
        note(&mut agent, "item/started", item.clone());
        note(&mut agent, "item/completed", item);
        assert_eq!(
            agent.items,
            [AgentItem::Compaction {
                status: ItemStatus::Done
            }]
        );
        assert_eq!(agent.compacting, None);
    }

    fn subagent(
        agent: &Agent,
        index: usize,
    ) -> (
        ItemStatus,
        String,
        Vec<mux_core::protocol::SubagentStep>,
        String,
    ) {
        match &agent.items[index] {
            AgentItem::Subagent {
                status,
                activity,
                steps,
                result,
                ..
            } => (*status, activity.clone(), steps.clone(), result.clone()),
            other => panic!("not a subagent: {other:?}"),
        }
    }

    // shapes taken from what claude 2.1.312 streamed for two subagents, one in the background
    #[test]
    fn claude_subagents_get_cards_with_their_own_steps() {
        let mut agent = claude_agent();
        agent.handle(json!({ "type": "assistant", "message": { "content": [
            { "type": "tool_use", "id": "t1", "name": "Agent", "input": {
                "description": "List files", "prompt": "Use Glob", "subagent_type": "general-purpose" } },
            { "type": "tool_use", "id": "t2", "name": "Agent", "input": {
                "description": "Read a file", "prompt": "Use Read", "subagent_type": "general-purpose",
                "run_in_background": true } }
        ] } }));
        agent.handle(json!({ "type": "system", "subtype": "task_started", "task_id": "k2", "tool_use_id": "t2" }));
        agent.handle(json!({ "type": "assistant", "parent_tool_use_id": "t2", "message": { "content": [
            { "type": "tool_use", "id": "s1", "name": "Read", "input": { "file_path": "/tmp/a.jsonl" } }
        ] } }));
        let (status, activity, steps, _) = subagent(&agent, 1);
        assert_eq!(status, ItemStatus::Running);
        assert_eq!(activity, "Read /tmp/a.jsonl");
        assert_eq!(steps[0].status, ItemStatus::Running);
        agent.handle(
            json!({ "type": "user", "parent_tool_use_id": "t2", "message": { "content": [
            { "type": "tool_result", "tool_use_id": "s1", "content": "line" }
        ] } }),
        );
        // the background one's tool result only says it launched, so it keeps running
        agent.handle(json!({ "type": "user", "message": { "content": [
            { "type": "tool_result", "tool_use_id": "t2", "content": "Async agent launched successfully." }
        ] } }));
        assert_eq!(subagent(&agent, 1).0, ItemStatus::Running);
        agent.handle(json!({ "type": "user", "tool_use_result": { "content": [{ "type": "text", "text": "a.jsonl" }] },
            "message": { "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "[hand-back] a.jsonl" }] } }));
        let (status, _, _, result) = subagent(&agent, 0);
        assert_eq!((status, result.as_str()), (ItemStatus::Done, "a.jsonl"));
        agent.handle(
            json!({ "type": "assistant", "parent_tool_use_id": "t2", "message": { "content": [
            { "type": "text", "text": "The first line is a system init." }
        ] } }),
        );
        agent.handle(json!({ "type": "system", "subtype": "task_notification", "task_id": "k2", "tool_use_id": "t2", "status": "completed" }));
        let (status, activity, steps, result) = subagent(&agent, 1);
        assert_eq!(status, ItemStatus::Done);
        assert!(activity.is_empty());
        assert_eq!(steps[0].status, ItemStatus::Done);
        assert_eq!(result, "The first line is a system init.");
    }

    #[test]
    fn codex_subagent_threads_fill_their_card_and_leave_the_turn_alone() {
        let mut agent = agent();
        agent.thread_id = Some("main".into());
        agent.set_busy(true);
        note(
            &mut agent,
            "item/started",
            json!({ "threadId": "main", "item": {
            "id": "c1", "type": "collabAgentToolCall", "tool": "spawnAgent", "prompt": "Find the flaky test",
            "receiverThreadIds": ["child"], "agentsStates": {}, "status": "inProgress" } }),
        );
        note(
            &mut agent,
            "turn/completed",
            json!({ "threadId": "child", "turn": { "status": "completed" } }),
        );
        assert!(
            agent.busy,
            "a subagent's turn ending is not the main turn ending"
        );
        note(
            &mut agent,
            "item/started",
            json!({ "threadId": "child", "item": {
            "id": "x1", "type": "commandExecution", "command": "cargo test" } }),
        );
        note(
            &mut agent,
            "item/completed",
            json!({ "threadId": "child", "item": {
            "id": "x1", "type": "commandExecution", "status": "completed" } }),
        );
        note(
            &mut agent,
            "item/completed",
            json!({ "threadId": "main", "item": {
            "id": "c2", "type": "collabAgentToolCall", "tool": "wait",
            "agentsStates": { "child": { "status": "completed", "message": "It's session_expiry." } } } }),
        );
        let (status, _, steps, result) = subagent(&agent, 0);
        assert_eq!(status, ItemStatus::Done);
        assert_eq!(steps[0].name, "Bash");
        assert_eq!(steps[0].status, ItemStatus::Done);
        assert_eq!(result, "It's session_expiry.");
    }

    #[test]
    fn sums_up_the_conversation_for_the_sidebar() {
        let mut agent = agent();
        agent.settings.model = Some("gpt-6.1-sol".into());
        agent.items = vec![
            AgentItem::User {
                text: "fix it".into(),
                attachments: Vec::new(),
                queued: false,
                rewind: None,
            },
            AgentItem::FileChange {
                changes: vec![FileChange {
                    path: "a.rs".into(),
                    diff: "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1,2 @@\n-old\n+new\n+more".into(),
                }],
                status: ItemStatus::Done,
            },
            AgentItem::Assistant {
                text: "Fixed the flaky test.".into(),
            },
        ];
        let summary = agent.summary();
        assert_eq!(summary.model, "gpt-6.1-sol");
        assert_eq!(summary.preview, "Fixed the flaky test.");
        assert_eq!(
            (
                summary.messages,
                summary.files,
                summary.added,
                summary.removed
            ),
            (1, 1, 2, 1)
        );
    }

    // shapes from codex's request_user_input schema and claude's AskUserQuestion tool
    #[test]
    fn questions_wait_for_an_answer_from_either_agent() {
        let mut agent = agent();
        agent.handle(json!({ "id": 4, "method": "item/tool/requestUserInput", "params": {
            "itemId": "i", "threadId": "t", "turnId": "u", "isBlocking": true,
            "questions": [{ "id": "q1", "header": "Approach", "question": "Which way?",
                "options": [{ "label": "Fast", "description": "Skip tests" }, { "label": "Safe", "description": "Run tests" }] }]
        } }));
        assert_eq!(agent.status(), SessionStatus::Waiting);
        let AgentItem::Question {
            id,
            questions,
            answers: None,
        } = agent.items[0].clone()
        else {
            panic!("expected an open question");
        };
        assert_eq!(questions[0].options[1].label, "Safe");
        agent.answer(id, vec![vec!["Safe".into()]]);
        assert!(
            matches!(&agent.items[0], AgentItem::Question { answers: Some(a), .. } if a[0] == ["Safe"])
        );
        assert_ne!(agent.status(), SessionStatus::Waiting);

        let mut claude = claude_agent();
        claude.handle(json!({ "type": "control_request", "request_id": "r1", "request": {
            "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": { "questions": [
                { "question": "Which files?", "header": "Scope", "multiSelect": true,
                  "options": [{ "label": "src", "description": "" }, { "label": "tests", "description": "" }] }
            ] } } }));
        assert!(
            matches!(&claude.items[0], AgentItem::Question { questions, .. } if questions[0].multiple)
        );
        // a turn that ends without an answer leaves the question marked unanswered
        claude.handle(json!({ "type": "result", "subtype": "success", "result": "ok" }));
        assert!(
            matches!(&claude.items[0], AgentItem::Question { answers: Some(a), .. } if a.is_empty())
        );
    }

    #[test]
    fn plans_are_caught_and_checklists_update_in_place() {
        let mut claude = claude_agent();
        claude.handle(json!({ "type": "control_request", "request_id": "r2", "request": {
            "subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": { "plan": "1. Read\n2. Fix" } } }));
        assert_eq!(
            claude.items,
            [AgentItem::Plan {
                text: "1. Read\n2. Fix".into()
            }]
        );
        assert!(
            claude.approvals.is_empty(),
            "a plan isn't a permission prompt"
        );

        let todos = |status: &str| {
            json!({ "type": "assistant", "message": { "content": [{
            "type": "tool_use", "id": format!("t-{status}"), "name": "TodoWrite", "input": { "todos": [
                { "content": "Read", "status": "completed", "activeForm": "Reading" },
                { "content": "Fix", "status": status, "activeForm": "Fixing" }
            ] } }] } })
        };
        claude.handle(todos("in_progress"));
        claude.handle(todos("completed"));
        let lists: Vec<_> = claude
            .items
            .iter()
            .filter(|i| matches!(i, AgentItem::Todos { .. }))
            .collect();
        assert_eq!(lists.len(), 1, "one checklist per turn");
        assert!(
            matches!(lists[0], AgentItem::Todos { steps } if steps[1].status == TodoStatus::Done)
        );

        let mut codex = agent();
        note(
            &mut codex,
            "turn/plan/updated",
            json!({ "threadId": "t", "turnId": "u", "plan": [
            { "step": "Read", "status": "inProgress" }, { "step": "Fix", "status": "pending" }
        ] }),
        );
        assert!(
            matches!(&codex.items[0], AgentItem::Todos { steps } if steps[0].status == TodoStatus::Active)
        );
    }

    #[test]
    fn messages_sent_mid_turn_wait_with_mux_and_go_one_per_turn() {
        let mut agent = claude_agent();
        let (tx, _rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.set_busy(true);
        let queue = |agent: &mut Agent, text: &str| {
            agent.queue.push(Queued {
                text: text.into(),
                attachments: Vec::new(),
            })
        };
        queue(&mut agent, "first");
        queue(&mut agent, "second");
        assert_eq!(
            agent.change_queue(1, QueueAction::Edit("second, edited".into())),
            None
        );
        let result = json!({ "type": "result", "subtype": "success", "result": "ok" });
        agent.handle(result.clone());
        // the turn ending sends just the next one
        assert!(
            matches!(agent.items.last(), Some(AgentItem::User { text, .. }) if text == "first")
        );
        assert_eq!(agent.queue.len(), 1);
        // a stop holds what's left
        agent.stop();
        assert!(agent.held);
        agent.claude_waiting.clear();
        agent.handle(result.clone());
        assert_eq!(agent.queue.len(), 1, "held messages don't go out");
        agent.resume();
        assert!(
            matches!(agent.items.last(), Some(AgentItem::User { text, .. }) if text == "second, edited")
        );
        assert!(agent.queue.is_empty());
        // removing drops it without sending
        queue(&mut agent, "never");
        agent.change_queue(0, QueueAction::Remove);
        assert!(agent.queue.is_empty());
    }

    // claude 2.1.312 read a message written during a running command right after that command's result
    #[test]
    fn waiting_messages_go_to_claude_at_its_next_tool_call() {
        let mut agent = claude_agent();
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.set_busy(true);
        agent.queue.push(Queued {
            text: "also check the tests".into(),
            attachments: Vec::new(),
        });
        let tool = json!({ "type": "assistant", "message": { "content": [
            { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "sleep 8" } }
        ] } });
        agent.handle(tool.clone());
        assert!(agent.queue.is_empty());
        assert!(
            matches!(agent.items.last(), Some(AgentItem::User { text, .. }) if text == "also check the tests")
        );
        let sent: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|line| serde_json::from_str(&line).unwrap())
            .collect();
        assert!(sent.iter().any(|m| m["type"] == "user"));

        // a queue held by a stop stays put
        agent.queue.push(Queued {
            text: "later".into(),
            attachments: Vec::new(),
        });
        agent.held = true;
        agent.handle(tool);
        assert_eq!(agent.queue.len(), 1);
    }

    #[test]
    fn rewinding_cuts_the_chat_and_points_each_agent_back() {
        let user = |rewind: Option<&str>| AgentItem::User {
            text: "go".into(),
            attachments: Vec::new(),
            queued: false,
            rewind: rewind.map(str::to_owned),
        };
        let reply = AgentItem::Assistant {
            text: "done".into(),
        };

        let mut claude = claude_agent();
        claude.session_id = Some("s1".into());
        claude.items = vec![
            user(Some("")),
            reply.clone(),
            user(Some("r1")),
            reply.clone(),
        ];
        assert!(claude.rewind(1));
        assert_eq!(claude.items, [user(Some("")), reply.clone()]);
        assert_eq!(claude.resume_at.as_deref(), Some("r1"));
        let args = claude_args(&claude.settings, Some("s1"), Some("r1")).join(" ");
        assert!(args.contains("--resume s1 --resume-session-at r1"));
        // back to before the first message starts claude afresh
        assert!(claude.rewind(0));
        assert!(claude.items.is_empty());
        assert_eq!(claude.session_id, None);

        let mut codex = agent();
        codex.items = vec![user(None), reply.clone()];
        note(
            &mut codex,
            "turn/started",
            json!({ "turn": { "id": "turn-1" } }),
        );
        assert_eq!(codex.items[0], user(Some("turn-1")));
        codex.set_busy(false);
        codex.thread_id = Some("th".into());
        assert!(codex.rewind(0));
        assert_eq!(codex.pending_revert.as_deref(), Some("turn-1"));

        codex.set_busy(true);
        assert!(!codex.rewind(0), "nothing is rewound while the agent works");
    }

    #[test]
    fn claude_messages_sent_mid_turn_wait_their_turn() {
        let mut agent = claude_agent();
        let user = |queued| AgentItem::User {
            text: String::new(),
            attachments: Vec::new(),
            queued,
            rewind: None,
        };
        agent.items = vec![user(false), user(true), user(true)];
        agent.claude_waiting = VecDeque::from(["a".to_owned(), "b".to_owned()]);
        agent.set_busy(true);
        let echo = |text: &str| json!({ "type": "user", "isReplay": true, "message": { "role": "user", "content": text } });
        let result = json!({ "type": "result", "subtype": "success", "result": "ok" });
        agent.handle(result.clone());
        agent.handle(echo("a"));
        assert_eq!(agent.items, [user(false), user(false), user(true)]);
        agent.handle(result.clone());
        agent.handle(echo("b"));
        agent.handle(result);
        assert_eq!(agent.items, [user(false), user(false), user(false)]);
        assert!(!agent.busy);
    }

    #[test]
    fn a_steer_that_misses_the_turn_starts_its_own() {
        let mut agent = agent();
        agent.thread_id = Some("th".into());
        agent
            .calls
            .insert(4, Call::Steer("also this".into(), Vec::new()));
        agent.handle(json!({ "id": 4, "error": { "message": "no active turn" } }));
        assert!(
            agent.calls.values().any(|c| matches!(c, Call::StartTurn)),
            "the message goes out as a new turn"
        );
    }

    // the message order below is what codex 0.164 sent for a turn that ran one approved command
    #[test]
    fn maps_a_turn_with_an_approved_command() {
        let mut agent = agent();
        note(
            &mut agent,
            "turn/started",
            json!({ "turn": { "id": "t1" } }),
        );
        note(
            &mut agent,
            "item/started",
            json!({ "item": { "type": "userMessage", "id": "u1" } }),
        );
        note(
            &mut agent,
            "item/started",
            json!({ "item": { "type": "agentMessage", "id": "m1", "text": "" } }),
        );
        note(
            &mut agent,
            "item/agentMessage/delta",
            json!({ "itemId": "m1", "delta": "I'll" }),
        );
        note(
            &mut agent,
            "item/agentMessage/delta",
            json!({ "itemId": "m1", "delta": " create it" }),
        );
        assert_eq!(
            agent.items[0],
            AgentItem::Assistant {
                text: "I'll create it".into()
            }
        );
        note(
            &mut agent,
            "item/completed",
            json!({ "item": { "type": "agentMessage", "id": "m1", "text": "I'll create it.\n" } }),
        );

        note(
            &mut agent,
            "item/started",
            json!({ "item": {
            "type": "commandExecution", "id": "c1", "command": "bash -lc \"printf hi > hello.txt\"", "status": "inProgress"
        } }),
        );
        agent.handle(json!({ "id": 0, "method": "item/commandExecution/requestApproval", "params": { "itemId": "c1", "kind": "command" } }));
        assert_eq!(agent.status(), SessionStatus::Waiting);
        let AgentItem::Approval { id, detail, .. } = &agent.items[2] else {
            panic!("expected an approval, got {:?}", agent.items[2]);
        };
        assert_eq!(detail, "bash -lc \"printf hi > hello.txt\"");
        agent.decide(*id, Decision::Allow);
        assert_eq!(agent.status(), SessionStatus::Running);

        note(
            &mut agent,
            "item/commandExecution/outputDelta",
            json!({ "itemId": "c1", "delta": "partial" }),
        );
        note(
            &mut agent,
            "item/completed",
            json!({ "item": {
            "type": "commandExecution", "id": "c1", "status": "completed", "exitCode": 0, "aggregatedOutput": "hi\n"
        } }),
        );
        note(
            &mut agent,
            "turn/completed",
            json!({ "turn": { "id": "t1", "status": "completed" } }),
        );

        assert_eq!(
            agent.items,
            vec![
                AgentItem::Assistant {
                    text: "I'll create it.\n".into()
                },
                AgentItem::Command {
                    command: "bash -lc \"printf hi > hello.txt\"".into(),
                    output: "hi\n".into(),
                    exit_code: Some(0),
                    status: ItemStatus::Done,
                    background: false,
                },
                AgentItem::Approval {
                    id: 1,
                    title: "Run a command?".into(),
                    detail: "bash -lc \"printf hi > hello.txt\"".into(),
                    decision: Some(Decision::Allow),
                },
            ]
        );
        assert!(!agent.busy);
        assert_eq!(agent.status(), SessionStatus::Done);
    }

    #[test]
    fn nonzero_exit_fails_and_unknown_requests_are_refused() {
        let mut agent = agent();
        note(
            &mut agent,
            "item/started",
            json!({ "item": { "type": "commandExecution", "id": "c1", "command": "false" } }),
        );
        note(
            &mut agent,
            "item/completed",
            json!({ "item": { "type": "commandExecution", "id": "c1", "status": "completed", "exitCode": 1 } }),
        );
        assert!(matches!(
            agent.items[0],
            AgentItem::Command {
                status: ItemStatus::Failed,
                exit_code: Some(1),
                ..
            }
        ));

        agent.handle(json!({ "id": 7, "method": "mcpServer/elicitation/request", "params": {} }));
        assert_eq!(
            agent.items.len(),
            1,
            "unsupported requests must not show up as approvals"
        );
    }

    #[test]
    fn turns_carry_the_chosen_model_effort_and_speed() {
        let defaults = turn_params("t", "hi", &[], &AgentSettings::default());
        assert!(defaults.get("model").is_none() && defaults.get("effort").is_none());
        assert_eq!(defaults["serviceTierForTurn"], "default");

        let chosen = AgentSettings {
            model: Some("gpt-5.6-luna".into()),
            effort: Some("high".into()),
            fast: true,
            ..Default::default()
        };
        let params = turn_params("t", "hi", &[], &chosen);
        assert_eq!(params["model"], "gpt-5.6-luna");
        assert_eq!(params["effort"], "high");
        assert_eq!(params["serviceTierForTurn"], "priority");
        assert_eq!(params["input"][0]["text"], "hi");
    }

    #[test]
    fn attachments_go_to_codex_as_images_and_mentions() {
        let files = ["/p/shot.PNG".to_owned(), "/p/notes.md".to_owned()];
        let input = &turn_params("t", "look", &files, &AgentSettings::default())["input"];
        assert_eq!(input[0]["text"], "look");
        assert_eq!(
            input[1],
            json!({ "type": "localImage", "path": "/p/shot.PNG" })
        );
        assert_eq!(
            input[2],
            json!({ "type": "mention", "name": "notes.md", "path": "/p/notes.md" })
        );
    }

    #[test]
    fn attachments_go_to_claude_inline_or_as_paths() {
        let dir = std::env::temp_dir().join(format!("mux-attach-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let image = dir.join("dot.png");
        fs::write(&image, b"png").unwrap();
        let image = image.to_string_lossy().into_owned();
        let content = claude_content("look", &[image, "/p/notes.md".to_owned()], &[]);
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "cG5n");
        assert_eq!(content[1]["text"], "look\n\nAttached files:\n- /p/notes.md");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn turns_carry_the_access_mode() {
        let defaults = turn_params("t", "hi", &[], &AgentSettings::default());
        assert_eq!(defaults["approvalPolicy"], "on-request");
        assert_eq!(defaults["sandboxPolicy"]["type"], "workspaceWrite");

        let full = AgentSettings {
            access: Access::Full,
            ..Default::default()
        };
        let params = turn_params("t", "hi", &[], &full);
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandboxPolicy"]["type"], "dangerFullAccess");

        let supervised = AgentSettings {
            access: Access::Supervised,
            ..Default::default()
        };
        assert_eq!(
            turn_params("t", "hi", &[], &supervised)["approvalPolicy"],
            "untrusted"
        );
    }

    fn claude_agent() -> Agent {
        let mut agent = agent();
        agent.settings.provider = Provider::Claude;
        agent
    }

    #[test]
    fn tracks_how_full_the_context_window_is() {
        let mut agent = agent();
        agent.handle(json!({ "method": "thread/tokenUsage/updated", "params": { "tokenUsage": {
            "last": { "totalTokens": 24000 }, "total": { "totalTokens": 90000 }, "modelContextWindow": 272000
        } } }));
        assert_eq!(
            agent.take_context(),
            Some(ContextUsage {
                used: 24000,
                window: 272000
            })
        );
        assert_eq!(agent.take_context(), None);

        // the numbers are what claude 2.1.312 reported for a one word haiku reply
        let mut claude = claude_agent();
        claude.handle(json!({ "type": "assistant", "message": { "content": [], "usage": {
            "input_tokens": 2, "cache_creation_input_tokens": 12223, "cache_read_input_tokens": 11383, "output_tokens": 4
        } } }));
        claude.handle(
            json!({ "type": "result", "subtype": "success", "result": "ok",
            "modelUsage": { "claude-haiku-5-5": { "contextWindow": 1000000 } } }),
        );
        assert_eq!(
            claude.take_context(),
            Some(ContextUsage {
                used: 23612,
                window: 1000000
            })
        );
    }

    // the frame order below is what claude 2.1.312 sent for a turn whose bash call was denied
    #[test]
    fn maps_a_claude_turn_with_a_denied_command() {
        let mut agent = claude_agent();
        agent.handle(json!({ "type": "system", "subtype": "init", "session_id": "s1" }));
        agent.handle(json!({ "type": "assistant", "message": { "content": [
            { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "touch denied.txt" } }
        ] } }));
        agent.handle(json!({ "type": "control_request", "request_id": "r1", "request": {
            "subtype": "can_use_tool", "tool_name": "Bash", "input": { "command": "touch denied.txt" },
            "permission_suggestions": [{ "type": "addRules", "destination": "localSettings" }]
        } }));
        assert_eq!(agent.status(), SessionStatus::Waiting);
        let AgentItem::Approval { id, detail, .. } = agent.items[1].clone() else {
            panic!("expected an approval, got {:?}", agent.items[1]);
        };
        assert_eq!(detail, "touch denied.txt");
        agent.decide(id, Decision::Deny);

        agent.handle(json!({ "type": "user", "message": { "content": [
            { "type": "tool_result", "tool_use_id": "t1", "content": "The user declined this in Mux.", "is_error": true }
        ] } }));
        agent.handle(json!({ "type": "stream_event", "event": { "type": "content_block_start", "content_block": { "type": "text" } } }));
        agent.handle(json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "Declined" } } }));
        assert_eq!(
            agent.items[2],
            AgentItem::Assistant {
                text: "Declined".into()
            }
        );
        agent.handle(json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "Declined, so I left it." }] } }));
        agent.handle(json!({ "type": "result", "subtype": "success", "is_error": false }));

        assert_eq!(agent.session_id.as_deref(), Some("s1"));
        assert!(
            matches!(&agent.items[0], AgentItem::Command { status: ItemStatus::Failed, output, .. } if output.contains("declined"))
        );
        assert!(matches!(
            agent.items[1],
            AgentItem::Approval {
                decision: Some(Decision::Deny),
                ..
            }
        ));
        assert_eq!(
            agent.items[2],
            AgentItem::Assistant {
                text: "Declined, so I left it.".into()
            }
        );
        assert_eq!(
            agent.items.len(),
            3,
            "the streamed reply and its final copy are one item"
        );
        assert!(!agent.busy);
    }

    #[test]
    fn claude_edits_become_diffs_and_always_allow_stays_in_the_session() {
        let mut agent = claude_agent();
        agent.handle(json!({ "type": "assistant", "message": { "content": [
            { "type": "tool_use", "id": "e1", "name": "Edit", "input": { "file_path": "src/a.rs", "old_string": "old", "new_string": "new" } },
            { "type": "tool_use", "id": "g1", "name": "Grep", "input": { "pattern": "SessionStore" } }
        ] } }));
        let AgentItem::FileChange { changes, .. } = &agent.items[0] else {
            panic!()
        };
        assert_eq!(changes[0].path, "src/a.rs");
        assert_eq!(changes[0].diff, "@@\n-old\n+new\n");
        assert!(
            matches!(&agent.items[1], AgentItem::Tool { name, detail, .. } if name == "Grep" && detail == "SessionStore")
        );

        let answer = claude_answer(
            Decision::AlwaysAllow,
            json!({ "command": "ls" }),
            &json!([{ "type": "addRules", "destination": "localSettings" }]),
        );
        assert_eq!(answer["updatedPermissions"][0]["destination"], "session");
        assert_eq!(answer["updatedInput"]["command"], "ls");
    }

    #[test]
    fn claude_launches_with_the_chosen_settings_and_session() {
        let settings = AgentSettings {
            provider: Provider::Claude,
            model: Some("opus".into()),
            effort: Some("high".into()),
            fast: true,
            access: Access::AutoEdits,
            worktree: false,
            plan: false,
        };
        let args = claude_args(&settings, Some("s1"), None).join(" ");
        assert!(args.contains("--permission-prompt-tool stdio"));
        assert!(args.contains("--permission-mode acceptEdits"));
        assert!(args.contains("--model opus") && args.contains("--effort high"));
        assert!(args.contains(r#"--settings {"fastMode":true}"#));
        assert!(args.ends_with("--resume s1"));

        let full = AgentSettings {
            access: Access::Full,
            ..settings
        };
        let args = claude_args(&full, None, None).join(" ");
        assert!(
            args.contains(
                "--permission-mode bypassPermissions --allow-dangerously-skip-permissions"
            )
        );
    }

    #[test]
    fn a_dropped_turn_expires_its_approvals() {
        let mut agent = agent();
        agent.handle(json!({ "id": 0, "method": "item/fileChange/requestApproval", "params": { "itemId": "missing" } }));
        assert_eq!(agent.status(), SessionStatus::Waiting);
        note(
            &mut agent,
            "turn/completed",
            json!({ "turn": { "status": "interrupted" } }),
        );
        assert!(matches!(
            agent.items[0],
            AgentItem::Approval {
                decision: Some(Decision::Deny),
                ..
            }
        ));
        assert_ne!(agent.status(), SessionStatus::Waiting);
    }
}
