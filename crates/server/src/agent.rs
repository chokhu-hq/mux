use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use mux_core::SessionStatus;
use mux_core::protocol::{AgentItem, AgentSettings, Decision, FileChange, ItemStatus};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{self, UnboundedSender};

use crate::Event;

// long command output is cut down to its tail, which is the part people read
const OUTPUT_LIMIT: usize = 32 * 1024;

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    thread_id: Option<String>,
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

struct Codex {
    stdin: UnboundedSender<String>,
    // held so the process is killed when the agent is dropped or restarted
    _child: Child,
}

// one coding agent conversation, driven through `codex app-server` over json-rpc on stdio
pub struct Agent {
    pub generation: u64,
    pub items: Vec<AgentItem>,
    pub busy: bool,
    pub settings: AgentSettings,
    thread_id: Option<String>,
    thread_live: bool,
    turn_id: Option<String>,
    cwd: Option<PathBuf>,
    codex: Option<Codex>,
    queued: Vec<String>,
    next_call: u64,
    calls: HashMap<u64, Call>,
    by_codex_id: HashMap<String, usize>,
    approvals: HashMap<u64, (Value, usize)>,
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
            thread_id: saved.thread_id,
            thread_live: false,
            turn_id: None,
            cwd: None,
            codex: None,
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
        if self.codex.is_none()
            && let Err(err) = self.start(session, generation, cwd, events)
        {
            let message = if err.kind() == io::ErrorKind::NotFound {
                "The Codex CLI wasn't found on PATH. Install it, then send again.".to_owned()
            } else {
                format!("Couldn't start Codex: {err}")
            };
            self.push(AgentItem::Error { message });
            self.set_busy(false);
            return;
        }
        if self.thread_live {
            self.start_turn(text);
        } else {
            self.queued.push(text);
        }
    }

    pub fn error(&mut self, message: String) {
        self.push(AgentItem::Error { message });
    }

    pub fn decide(&mut self, approval: u64, decision: Decision) {
        let Some((request, index)) = self.approvals.remove(&approval) else {
            return;
        };
        let answer = match decision {
            Decision::Allow => "accept",
            Decision::AlwaysAllow => "acceptForSession",
            Decision::Deny => "decline",
        };
        self.write(json!({ "id": request, "result": { "decision": answer } }));
        if let Some(AgentItem::Approval { decision: d, .. }) = self.items.get_mut(index) {
            *d = Some(decision);
            self.changed.insert(index);
        }
    }

    pub fn stop(&mut self) {
        if let (Some(thread), Some(turn)) = (self.thread_id.clone(), self.turn_id.clone()) {
            self.call(
                Call::Interrupt,
                "turn/interrupt",
                json!({ "threadId": thread, "turnId": turn }),
            );
        }
    }

    pub fn exited(&mut self) {
        self.codex = None;
        self.thread_live = false;
        self.calls.clear();
        if self.busy {
            self.push(AgentItem::Error {
                message: "Codex stopped unexpectedly.".to_owned(),
            });
        }
        self.expire_approvals();
        self.turn_id = None;
        self.set_busy(false);
    }

    fn start(
        &mut self,
        session: u64,
        generation: u64,
        cwd: PathBuf,
        events: &UnboundedSender<Event>,
    ) -> io::Result<()> {
        let mut command = Command::new("codex");
        command
            .arg("app-server")
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
                let event = Event::Codex {
                    session,
                    generation,
                    msg,
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            let _ = events.send(Event::CodexExited {
                session,
                generation,
            });
        });

        self.generation = generation;
        self.codex = Some(Codex {
            stdin: tx,
            _child: child,
        });
        self.cwd = Some(cwd);
        self.call(
            Call::Initialize,
            "initialize",
            json!({ "clientInfo": { "name": "mux", "title": "Mux", "version": mux_core::VERSION } }),
        );
        Ok(())
    }

    pub fn handle(&mut self, msg: Value) {
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
        self.approvals.insert(approval, (id, index));
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
        for (_, (_, index)) in self.approvals.drain() {
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
        if let Some(codex) = &self.codex {
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
    params
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
        };
        let params = turn_params("t", "hi", &chosen);
        assert_eq!(params["model"], "gpt-5.6-luna");
        assert_eq!(params["effort"], "high");
        assert_eq!(params["serviceTierForTurn"], "priority");
        assert_eq!(params["input"][0]["text"], "hi");
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
