use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use base64::Engine;
use mux_core::protocol::{
    AgentItem, AgentSettings, AgentSummary, ContextUsage, Decision, FileChange, ItemStatus, Skill,
    UsageWindow, is_image,
};
use mux_core::{Access, Provider, SessionStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::{Event, usage};

mod interaction;
mod subagent;

// long command output is cut down to its tail, which is the part people read
const OUTPUT_LIMIT: usize = 32 * 1024;

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
}

enum Call {
    Initialize,
    StartThread,
    ResumeThread,
    StartTurn,
    // input added to a running turn, kept so it can start its own turn if that one already ended
    Steer(String, Vec<String>),
    Interrupt,
    Compact,
}

struct Process {
    stdin: UnboundedSender<String>,
    // held so the process is killed when the agent is dropped or restarted
    _child: Child,
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
    // the settings claude was launched with, since changing them means relaunching
    launched: Option<AgentSettings>,
    queued: Vec<(String, Vec<String>)>,
    next_call: u64,
    calls: HashMap<u64, Call>,
    by_codex_id: HashMap<String, usize>,
    approvals: HashMap<u64, Pending>,
    next_approval: u64,
    changed: BTreeSet<usize>,
    busy_changed: bool,
    // messages claude hasn't echoed back yet, which it may fold into one turn when they arrive close together
    claude_waiting: VecDeque<String>,
    // claude is mid turn, whether answering a message or one it started itself, like a background subagent waking
    claude_turn: bool,
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
    // the compaction item still running, whether asked for or started by the agent itself
    compacting: Option<usize>,
    compact_queued: bool,
}

impl Agent {
    pub fn load(path: &Path) -> Self {
        let saved: Saved = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
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
            launched: None,
            queued: Vec::new(),
            next_call: 0,
            calls: HashMap::new(),
            by_codex_id: HashMap::new(),
            approvals: HashMap::new(),
            next_approval: 0,
            changed: BTreeSet::new(),
            busy_changed: false,
            claude_waiting: VecDeque::new(),
            claude_turn: false,
            sub_steps: HashMap::new(),
            tasks: HashMap::new(),
            child_threads: HashMap::new(),
            questions: HashMap::new(),
            todos: None,
            default_model: None,
            compacting: None,
            compact_queued: false,
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
    pub fn take_changes(&mut self) -> (Vec<(usize, AgentItem)>, Option<bool>) {
        let items = std::mem::take(&mut self.changed)
            .into_iter()
            .filter_map(|i| Some((i, self.items.get(i)?.clone())))
            .collect();
        let busy = std::mem::take(&mut self.busy_changed).then_some(self.busy);
        (items, busy)
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
        let was_busy = self.busy;
        // a new message starts a new checklist
        self.todos = None;
        // claude runs a message sent mid turn after the current one, and codex waits for its thread to open
        let claude = self.settings.provider == Provider::Claude;
        let queued = if claude {
            was_busy
        } else {
            !self.thread_live && self.process.is_some()
        };
        self.push(AgentItem::User {
            text: text.clone(),
            attachments: attachments.clone(),
            queued,
        });
        self.set_busy(true);
        if !self.launch(was_busy, session, generation, cwd, events) {
            return;
        }
        if self.settings.provider == Provider::Claude {
            let line = json!({
                "type": "user",
                "message": { "role": "user", "content": claude_content(&text, &attachments, &self.skills()) }
            });
            self.claude_waiting.push_back(text.clone());
            self.write(line);
        } else if let (true, true, Some(thread), Some(turn)) = (
            was_busy,
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

    // summarises the conversation so far, which only makes sense between turns of a started one
    pub fn compact(
        &mut self,
        session: u64,
        generation: u64,
        cwd: PathBuf,
        events: &UnboundedSender<Event>,
    ) {
        let claude = self.settings.provider == Provider::Claude;
        let started = if claude {
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
        // claude bakes model, effort and speed in at launch, so new settings mean relaunching with --resume,
        // which waits for a quiet moment rather than cutting off a turn in progress
        if provider == Provider::Claude && !busy && self.launched.as_ref() != Some(&self.settings) {
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

    pub fn stop(&mut self) {
        if self.settings.provider == Provider::Claude {
            if self.busy {
                self.write(json!({
                    "type": "control_request",
                    "request_id": format!("mux-stop-{}", self.next_call),
                    "request": { "subtype": "interrupt" }
                }));
                self.next_call += 1;
            }
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
        self.process = None;
        self.thread_live = false;
        self.calls.clear();
        if self.busy {
            self.push(AgentItem::Error {
                message: format!("{} stopped unexpectedly.", self.settings.provider.label()),
            });
        }
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
        let mut command = if claude {
            let mut command = Command::new("claude");
            command.args(claude_args(&self.settings, self.session_id.as_deref()));
            command
        } else {
            let mut command = Command::new("codex");
            command.arg("app-server");
            command
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .current_dir(&cwd);
        let mut child = command.spawn()?;
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
        self.process = Some(Process {
            stdin: tx,
            _child: child,
        });
        self.cwd = Some(cwd);
        if claude {
            // claude needs no handshake, a user message on stdin is enough to start a turn
            self.launched = Some(self.settings.clone());
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
            Call::Interrupt => {}
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
                self.set_busy(true);
            }
            "turn/completed" => {
                let turn = &params["turn"];
                if turn["status"] == "failed" {
                    let message = turn["error"]["message"]
                        .as_str()
                        .unwrap_or("The turn failed.");
                    self.push(AgentItem::Error {
                        message: message.to_owned(),
                    });
                }
                self.turn_id = None;
                self.expire_approvals();
                self.expire_questions();
                self.finish_compaction(ItemStatus::Failed);
                self.set_busy(false);
            }
            "error" if params["willRetry"] != true => {
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
                self.usage = Some(usage::codex(&params["rateLimits"]));
            }
            "item/started" => self.item_started(&params["item"]),
            "item/completed" => self.item_completed(&params["item"]),
            "item/agentMessage/delta" => {
                if let Some(AgentItem::Assistant { text }) = self.target_mut(params) {
                    text.push_str(params["delta"].as_str().unwrap_or_default());
                }
            }
            "item/commandExecution/outputDelta" => {
                if let Some(AgentItem::Command { output, .. }) = self.target_mut(params) {
                    output.push_str(params["delta"].as_str().unwrap_or_default());
                    trim_front(output);
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
            // the compaction item has no status, so finishing it is what says it worked
            Some(AgentItem::Compaction { status: s }) => {
                *s = ItemStatus::Done;
                self.compacting = None;
            }
            _ => return,
        }
        self.changed.insert(index);
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
                self.claude_task(msg)
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
                let event = &msg["event"];
                match event["type"].as_str() {
                    Some("content_block_start") if event["content_block"]["type"] == "text" => {
                        let index = self.push(AgentItem::Assistant {
                            text: String::new(),
                        });
                        self.live_text = Some(index);
                    }
                    Some("content_block_delta") if event["delta"]["type"] == "text_delta" => {
                        if let Some(index) = self.live_text
                            && let Some(AgentItem::Assistant { text }) = self.items.get_mut(index)
                        {
                            text.push_str(event["delta"]["text"].as_str().unwrap_or_default());
                            self.changed.insert(index);
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                self.claude_turn = true;
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
                        Some("tool_use") => self.claude_tool(block),
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
            Some("rate_limit_event") => {
                self.usage = Some(usage::claude(&msg["rate_limit_info"]));
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
                if msg["is_error"] == true || msg["subtype"] != "success" {
                    let message = msg["result"]
                        .as_str()
                        .filter(|r| !r.is_empty())
                        .unwrap_or("Claude stopped with an error.");
                    self.push(AgentItem::Error {
                        message: message.to_owned(),
                    });
                }
                self.live_text = None;
                self.expire_approvals();
                self.expire_questions();
                self.finish_compaction(ItemStatus::Failed);
                self.claude_turn = false;
                // messages it hasn't echoed yet will start a turn of their own
                self.set_busy(!self.claude_waiting.is_empty());
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
        self.items.push(item);
        let index = self.items.len() - 1;
        self.changed.insert(index);
        index
    }

    fn set_busy(&mut self, busy: bool) {
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

fn claude_args(settings: &AgentSettings, session: Option<&str>) -> Vec<String> {
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
        // plan mode stands in for the access level until the plan is accepted
        match settings.access {
            _ if settings.plan => "plan",
            Access::Supervised => "manual",
            Access::AutoEdits => "acceptEdits",
            Access::Auto => "auto",
            Access::Full => "bypassPermissions",
        },
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
    }
    args
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

    fn note(agent: &mut Agent, method: &str, params: Value) {
        agent.handle(json!({ "method": method, "params": params }));
    }

    // the order claude 2.1.312 sent things in when two messages arrived during a turn and were answered together
    #[test]
    fn claude_is_done_when_merged_messages_get_one_answer() {
        let mut agent = claude_agent();
        let user = |text: &str| AgentItem::User {
            text: text.into(),
            attachments: Vec::new(),
            queued: false,
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
    fn claude_messages_sent_mid_turn_wait_their_turn() {
        let mut agent = claude_agent();
        let user = |queued| AgentItem::User {
            text: String::new(),
            attachments: Vec::new(),
            queued,
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
        let args = claude_args(&settings, Some("s1")).join(" ");
        assert!(args.contains("--permission-prompt-tool stdio"));
        assert!(args.contains("--permission-mode acceptEdits"));
        assert!(args.contains("--model opus") && args.contains("--effort high"));
        assert!(args.contains(r#"--settings {"fastMode":true}"#));
        assert!(args.ends_with("--resume s1"));

        let full = AgentSettings {
            access: Access::Full,
            ..settings
        };
        let args = claude_args(&full, None).join(" ");
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
