use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use mux_core::protocol::{AgentItem, AgentSettings, Decision, FileChange, ItemStatus, UsageWindow};
use mux_core::{Access, Provider, SessionStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::{Event, usage};

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
}

enum Call {
    Initialize,
    StartThread,
    ResumeThread,
    StartTurn,
    Interrupt,
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
    queued: Vec<String>,
    next_call: u64,
    calls: HashMap<u64, Call>,
    by_codex_id: HashMap<String, usize>,
    approvals: HashMap<u64, Pending>,
    next_approval: u64,
    changed: BTreeSet<usize>,
    busy_changed: bool,
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
                AgentItem::Command { status, .. } | AgentItem::FileChange { status, .. }
                    if *status == ItemStatus::Running =>
                {
                    *status = ItemStatus::Failed
                }
                AgentItem::Approval { decision, .. } if decision.is_none() => {
                    *decision = Some(Decision::Deny)
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
        }
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
        };
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_vec(&saved).map_err(io::Error::other)?)?;
        fs::rename(tmp, path)
    }

    pub fn status(&self) -> SessionStatus {
        if !self.approvals.is_empty() {
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
        session: u64,
        generation: u64,
        cwd: PathBuf,
        events: &UnboundedSender<Event>,
    ) {
        self.push(AgentItem::User { text: text.clone() });
        self.set_busy(true);
        let provider = self.settings.provider;
        // claude bakes model, effort and speed in at launch, so new settings mean relaunching with --resume
        if provider == Provider::Claude && self.launched.as_ref() != Some(&self.settings) {
            self.process = None;
        }
        if self.process.is_none()
            && let Err(err) = self.start(session, generation, cwd, events)
        {
            let name = provider.label();
            let message = if err.kind() == io::ErrorKind::NotFound {
                format!("The {name} CLI wasn't found on PATH. Install it, then send again.")
            } else {
                format!("Couldn't start {name}: {err}")
            };
            self.push(AgentItem::Error { message });
            self.set_busy(false);
            return;
        }
        if provider == Provider::Claude {
            let line = json!({
                "type": "user",
                "message": { "role": "user", "content": [{ "type": "text", "text": text }] }
            });
            self.write(line);
        } else if self.thread_live {
            self.start_turn(text);
        } else {
            self.queued.push(text);
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
        self.turn_id = None;
        self.live_text = None;
        self.launched = None;
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
            json!({ "clientInfo": { "name": "mux", "title": "Mux", "version": mux_core::VERSION } }),
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
                    let queued = std::mem::take(&mut self.queued);
                    if !queued.is_empty() {
                        self.start_turn(queued.join("\n\n"));
                    }
                }
            },
            Call::StartTurn => match error {
                Some(message) => self.fail(format!("Codex didn't start the turn: {message}")),
                None => self.turn_id = msg["result"]["turn"]["id"].as_str().map(str::to_owned),
            },
            Call::Interrupt => {}
        }
    }

    fn request(&mut self, method: &str, id: Value, params: &Value) {
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
            _ => return,
        };
        let index = self.push(entry);
        self.by_codex_id.insert(id, index);
    }

    fn item_completed(&mut self, item: &Value) {
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
            _ => return,
        }
        self.changed.insert(index);
    }

    fn handle_claude(&mut self, msg: &Value) {
        // subagent traffic carries a parent tool id, and only the main conversation is shown
        if msg["parent_tool_use_id"].is_string() {
            return;
        }
        match msg["type"].as_str() {
            Some("system") if msg["subtype"] == "init" => {
                self.session_id = msg["session_id"].as_str().map(str::to_owned);
            }
            Some("stream_event") => {
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
            Some("user") => {
                for block in msg["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        self.claude_tool_result(block);
                    }
                }
            }
            Some("control_request") => self.claude_request(msg),
            Some("rate_limit_event") => {
                self.usage = Some(usage::claude(&msg["rate_limit_info"]));
            }
            Some("result") => {
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
                self.set_busy(false);
            }
            _ => {}
        }
    }

    fn claude_tool(&mut self, block: &Value) {
        let Some(id) = block["id"].as_str().map(str::to_owned) else {
            return;
        };
        let input = &block["input"];
        let field = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
        let name = block["name"].as_str().unwrap_or_default();
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

    fn claude_tool_result(&mut self, block: &Value) {
        let Some(&index) = block["tool_use_id"]
            .as_str()
            .and_then(|id| self.by_codex_id.get(id))
        else {
            return;
        };
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

    fn start_turn(&mut self, text: String) {
        let Some(thread) = self.thread_id.clone() else {
            return;
        };
        let params = turn_params(&thread, &text, &self.settings);
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
fn turn_params(thread: &str, text: &str, settings: &AgentSettings) -> Value {
    let mut params = json!({ "threadId": thread, "input": [{ "type": "text", "text": text }] });
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
        // sends permission prompts to us over stdio instead of answering them inside the cli
        "--permission-prompt-tool",
        "stdio",
        "--permission-mode",
        match settings.access {
            Access::Supervised => "manual",
            Access::AutoEdits => "acceptEdits",
            Access::Auto => "auto",
            Access::Full => "bypassPermissions",
        },
    ]
    .map(str::to_owned)
    .into();
    // claude refuses to bypass permissions unless it is explicitly allowed to
    if settings.access == Access::Full {
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

    fn agent() -> Agent {
        Agent::load(Path::new("/nonexistent/mux-agent.json"))
    }

    fn note(agent: &mut Agent, method: &str, params: Value) {
        agent.handle(json!({ "method": method, "params": params }));
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

        agent.handle(json!({ "id": 7, "method": "item/tool/requestUserInput", "params": {} }));
        assert_eq!(
            agent.items.len(),
            1,
            "unsupported requests must not show up as approvals"
        );
    }

    #[test]
    fn turns_carry_the_chosen_model_effort_and_speed() {
        let defaults = turn_params("t", "hi", &AgentSettings::default());
        assert!(defaults.get("model").is_none() && defaults.get("effort").is_none());
        assert_eq!(defaults["serviceTierForTurn"], "default");

        let chosen = AgentSettings {
            model: Some("gpt-5.6-luna".into()),
            effort: Some("high".into()),
            fast: true,
            ..Default::default()
        };
        let params = turn_params("t", "hi", &chosen);
        assert_eq!(params["model"], "gpt-5.6-luna");
        assert_eq!(params["effort"], "high");
        assert_eq!(params["serviceTierForTurn"], "priority");
        assert_eq!(params["input"][0]["text"], "hi");
    }

    #[test]
    fn turns_carry_the_access_mode() {
        let defaults = turn_params("t", "hi", &AgentSettings::default());
        assert_eq!(defaults["approvalPolicy"], "on-request");
        assert_eq!(defaults["sandboxPolicy"]["type"], "workspaceWrite");

        let full = AgentSettings {
            access: Access::Full,
            ..Default::default()
        };
        let params = turn_params("t", "hi", &full);
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandboxPolicy"]["type"], "dangerFullAccess");

        let supervised = AgentSettings {
            access: Access::Supervised,
            ..Default::default()
        };
        assert_eq!(
            turn_params("t", "hi", &supervised)["approvalPolicy"],
            "untrusted"
        );
    }

    fn claude_agent() -> Agent {
        let mut agent = agent();
        agent.settings.provider = Provider::Claude;
        agent
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
