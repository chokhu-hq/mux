use std::fs;

use base64::Engine;
use mux_core::Access;
use mux_core::protocol::{
    AgentItem, ContextUsage, Decision, FileChange, ItemStatus, Skill, TodoStatus, TodoStep,
    is_image,
};
use serde_json::{Value, json};

use super::{Agent, Call, Pending, file_name, media_type, trim_front};

// settings confirmed by opencode, so each prompt only sends what changed
#[derive(Clone, Default, PartialEq)]
pub(super) struct Applied {
    model: Option<String>,
    effort: Option<String>,
    plan: bool,
}

// opencode is driven through the agent client protocol, json-rpc over the stdio of `opencode acp`
impl Agent {
    pub(super) fn opencode_initialize(&mut self) {
        self.acp_call(
            Call::Initialize,
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            }),
        );
    }

    pub(super) fn handle_opencode(&mut self, msg: &Value) {
        // opencode numbers its own requests, so a method is what tells them apart from replies
        match (msg["method"].as_str(), msg.get("id").cloned()) {
            (Some("session/request_permission"), Some(id)) => {
                self.opencode_permission(id, &msg["params"])
            }
            (Some(method), Some(id)) => {
                log::warn!("opencode asked for {method}, which mux doesn't support yet");
                self.write(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("{method} is not supported by mux") },
                }));
            }
            (Some("session/update"), None) => self.opencode_update(&msg["params"]["update"]),
            (Some(_), None) => {}
            (None, Some(id)) => self.opencode_response(id, msg),
            (None, None) => {}
        }
    }

    fn opencode_response(&mut self, id: Value, msg: &Value) {
        let Some(call) = id.as_u64().and_then(|id| self.calls.remove(&id)) else {
            return;
        };
        let error = msg
            .get("error")
            .map(|e| e["message"].as_str().unwrap_or("unknown error").to_owned());
        match call {
            Call::Initialize => self.opencode_open(),
            Call::ResumeThread if error.is_some() => {
                // opencode no longer has the old session, so carry on in a fresh one
                self.session_id = None;
                self.opencode_open();
            }
            Call::StartThread | Call::ResumeThread => match error {
                Some(message) => {
                    self.fail(format!("Couldn't start an OpenCode session: {message}"))
                }
                None => {
                    if let Some(id) = msg["result"]["sessionId"].as_str() {
                        self.session_id = Some(id.to_owned());
                    }
                    self.thread_live = true;
                    self.opencode_options(&msg["result"]["configOptions"]);
                    if std::mem::take(&mut self.compact_queued) {
                        self.opencode_prompt(json!([{ "type": "text", "text": "/compact" }]));
                    }
                    let queued = std::mem::take(&mut self.queued);
                    self.unqueue(usize::MAX);
                    if !queued.is_empty() {
                        let (texts, files): (Vec<_>, Vec<_>) = queued.into_iter().unzip();
                        self.opencode_send(texts.join("\n\n"), files.concat());
                    }
                }
            },
            Call::StartTurn => {
                match error {
                    Some(message) => {
                        self.finish_compaction(ItemStatus::Failed);
                        self.push(AgentItem::Error { message });
                    }
                    None => {
                        let done = match msg["result"]["stopReason"].as_str() {
                            Some("cancelled") => ItemStatus::Failed,
                            _ => ItemStatus::Done,
                        };
                        self.finish_compaction(done);
                        if let Some(reason) = refusal(&msg["result"]["stopReason"]) {
                            self.push(AgentItem::Error { message: reason });
                        }
                    }
                }
                self.live_text = None;
                self.expire_approvals();
                self.set_busy(false);
                self.next_queued();
            }
            Call::Configure(config, value) => {
                if let Some(message) = error {
                    self.pending_prompt = None;
                    self.finish_compaction(ItemStatus::Failed);
                    self.fail(format!("OpenCode didn't take the new settings: {message}"));
                } else {
                    match config {
                        "model" => self.applied.model = Some(value),
                        "effort" => self.applied.effort = Some(value),
                        "mode" => self.applied.plan = value == "plan",
                        _ => {}
                    }
                    if let Some(prompt) = self.pending_prompt.take() {
                        self.opencode_prompt(prompt);
                    }
                }
            }
            _ => {}
        }
    }

    fn opencode_open(&mut self) {
        let cwd = self.cwd.clone().unwrap_or_default();
        let params = json!({ "cwd": cwd, "mcpServers": [] });
        match self.session_id.clone() {
            Some(session) => {
                let mut params = params;
                params["sessionId"] = json!(session);
                self.acp_call(Call::ResumeThread, "session/resume", params);
            }
            None => self.acp_call(Call::StartThread, "session/new", params),
        }
    }

    // the session's starting model, effort and mode, so only a change made in mux is sent
    fn opencode_options(&mut self, options: &Value) {
        let current = |id: &str| {
            options
                .as_array()
                .into_iter()
                .flatten()
                .find(|o| o["id"] == id)
                .and_then(|o| o["currentValue"].as_str())
                .map(str::to_owned)
        };
        self.applied = Applied {
            model: current("model"),
            effort: current("effort"),
            plan: current("mode").as_deref() == Some("plan"),
        };
    }

    pub(super) fn opencode_send(&mut self, text: String, attachments: Vec<String>) {
        let text = command_text(&text, &self.skills());
        self.opencode_prompt(prompt_blocks(&text, &attachments));
    }

    pub(super) fn opencode_compact(&mut self) {
        if self.thread_live {
            self.opencode_prompt(json!([{ "type": "text", "text": "/compact" }]));
        } else {
            self.compact_queued = true;
        }
    }

    fn opencode_prompt(&mut self, prompt: Value) {
        let Some(session) = self.session_id.clone() else {
            return;
        };
        if self
            .calls
            .values()
            .any(|call| matches!(call, Call::Configure(..)))
        {
            self.pending_prompt = Some(prompt);
            return;
        }
        let settings = &self.settings;
        let wanted = Applied {
            model: settings.model.clone().or(self.applied.model.clone()),
            effort: settings.effort.clone().or(self.applied.effort.clone()),
            plan: settings.plan,
        };
        // the model goes first, since the efforts on offer depend on it
        let mut changes = Vec::new();
        if wanted.model != self.applied.model
            && let Some(model) = &wanted.model
        {
            changes.push(("model", model.clone()));
        }
        if wanted.effort != self.applied.effort
            && let Some(effort) = &wanted.effort
        {
            changes.push(("effort", effort.clone()));
        }
        if wanted.plan != self.applied.plan {
            changes.push((
                "mode",
                if wanted.plan { "plan" } else { "build" }.to_owned(),
            ));
        }
        if let Some((config, value)) = changes.into_iter().next() {
            self.pending_prompt = Some(prompt);
            self.acp_call(
                Call::Configure(config, value.clone()),
                "session/set_config_option",
                json!({ "sessionId": session, "configId": config, "value": value }),
            );
            return;
        }
        self.live_text = None;
        self.acp_call(
            Call::StartTurn,
            "session/prompt",
            json!({ "sessionId": session, "prompt": prompt }),
        );
    }

    pub(super) fn opencode_stop(&mut self) {
        if let (true, Some(session)) = (self.busy, self.session_id.clone()) {
            self.write(json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session } }));
        }
    }

    fn opencode_update(&mut self, update: &Value) {
        match update["sessionUpdate"].as_str() {
            Some("agent_message_chunk") => {
                let chunk = update["content"]["text"].as_str().unwrap_or_default();
                match self.live_text {
                    Some(index) => self.append(index, chunk),
                    None => {
                        self.live_text = Some(self.push(AgentItem::Assistant {
                            text: chunk.to_owned(),
                        }));
                    }
                }
            }
            Some("agent_thought_chunk") => {
                self.reason(update["content"]["text"].as_str().unwrap_or_default());
            }
            Some("tool_call") => self.opencode_tool(update),
            Some("tool_call_update") => self.opencode_tool_update(update),
            Some("plan") => {
                let steps = update["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|e| TodoStep {
                        text: e["content"].as_str().unwrap_or_default().to_owned(),
                        status: match e["status"].as_str() {
                            Some("completed") => TodoStatus::Done,
                            Some("in_progress") => TodoStatus::Active,
                            _ => TodoStatus::Pending,
                        },
                    })
                    .collect();
                self.set_todos(steps);
            }
            Some("usage_update") => {
                if let (Some(used), Some(window)) =
                    (update["used"].as_u64(), update["size"].as_u64())
                {
                    self.set_context(ContextUsage { used, window });
                }
            }
            Some("available_commands_update") => {
                // compact already has its own entry in mux's menu
                let commands: Vec<Skill> = update["availableCommands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["name"] != "compact")
                    .filter_map(|c| {
                        Some(Skill {
                            name: c["name"].as_str()?.to_owned(),
                            description: c["description"].as_str().unwrap_or_default().to_owned(),
                            path: String::new(),
                            scope: "OpenCode".to_owned(),
                        })
                    })
                    .collect();
                if commands != self.found {
                    self.found = commands;
                    self.commands_changed = true;
                }
            }
            _ => {}
        }
    }

    fn opencode_tool(&mut self, update: &Value) {
        let Some(id) = update["toolCallId"].as_str().map(str::to_owned) else {
            return;
        };
        // text after a tool call is a new reply, not more of the one before it
        self.live_text = None;
        let entry = match update["kind"].as_str() {
            Some("execute") => AgentItem::Command {
                command: command_of(update),
                output: String::new(),
                exit_code: None,
                status: ItemStatus::Running,
                background: false,
            },
            Some("edit" | "delete" | "move") => AgentItem::FileChange {
                changes: Vec::new(),
                status: ItemStatus::Running,
            },
            _ => AgentItem::Tool {
                name: update["name"]
                    .as_str()
                    .or(update["title"].as_str())
                    .unwrap_or("tool")
                    .to_owned(),
                detail: detail_of(update),
                output: String::new(),
                status: ItemStatus::Running,
            },
        };
        let index = self.push(entry);
        self.by_codex_id.insert(id, index);
        self.opencode_tool_update(update);
    }

    fn opencode_tool_update(&mut self, update: &Value) {
        let Some(&index) = update["toolCallId"]
            .as_str()
            .and_then(|id| self.by_codex_id.get(id))
        else {
            return;
        };
        let status = match update["status"].as_str() {
            Some("completed") => Some(ItemStatus::Done),
            Some("failed") => Some(ItemStatus::Failed),
            _ => None,
        };
        let text = content_text(&update["content"]);
        let exit = update["rawOutput"]["metadata"]["exit"]
            .as_i64()
            .map(|c| c as i32);
        match self.items.get_mut(index) {
            Some(AgentItem::Command {
                command,
                output,
                exit_code,
                status: s,
                ..
            }) => {
                let named = command_of(update);
                if !named.is_empty() {
                    *command = named;
                }
                if let Some(text) = text {
                    *output = text;
                    trim_front(output);
                }
                if exit.is_some() {
                    *exit_code = exit;
                }
                if let Some(status) = status {
                    *s = match (status, *exit_code) {
                        (ItemStatus::Done, Some(code)) if code != 0 => ItemStatus::Failed,
                        (status, _) => status,
                    };
                }
            }
            Some(AgentItem::FileChange { changes, status: s }) => {
                let found = file_diffs(update);
                if !found.is_empty() {
                    *changes = found;
                } else if changes.is_empty() {
                    // until the diff arrives the paths at least say which files are being edited
                    *changes = locations(update)
                        .into_iter()
                        .map(|path| FileChange {
                            path,
                            diff: String::new(),
                        })
                        .collect();
                }
                if let Some(status) = status {
                    *s = status;
                }
            }
            Some(AgentItem::Tool {
                detail,
                output,
                status: s,
                ..
            }) => {
                let named = detail_of(update);
                if !named.is_empty() {
                    *detail = named;
                }
                if let Some(text) = text {
                    *output = text;
                    trim_front(output);
                }
                if let Some(status) = status {
                    *s = status;
                }
            }
            _ => return,
        }
        self.changed.insert(index);
    }

    fn opencode_permission(&mut self, id: Value, params: &Value) {
        let call = &params["toolCall"];
        let kind = call["kind"].as_str().unwrap_or_default();
        // opencode asks by its own rules, and mux's access level answers what it already allows
        let allowed = match self.settings.access {
            Access::Full => true,
            Access::AutoEdits | Access::Auto => kind == "edit",
            Access::Supervised => false,
        };
        if allowed {
            self.opencode_choose(&id, &params["options"], Decision::Allow);
            return;
        }
        let (title, detail) = match kind {
            "execute" => ("Run a command?".to_owned(), command_of(call)),
            "edit" | "delete" | "move" => {
                ("Apply file changes?".to_owned(), locations(call).join("\n"))
            }
            _ => {
                let name = call["name"]
                    .as_str()
                    .or(call["title"].as_str())
                    .unwrap_or("this tool");
                (format!("Allow {name}?"), detail_of(call))
            }
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
                input: params["options"].clone(),
                suggestions: Value::Null,
            },
        );
    }

    pub(super) fn opencode_choose(&self, id: &Value, options: &Value, decision: Decision) {
        let kinds: &[&str] = match decision {
            Decision::Allow => &["allow_once", "allow_always"],
            Decision::AlwaysAllow => &["allow_always", "allow_once"],
            Decision::Deny => &["reject_once", "reject_always"],
        };
        let option = kinds.iter().find_map(|kind| {
            options
                .as_array()
                .into_iter()
                .flatten()
                .find(|o| o["kind"] == *kind)
                .and_then(|o| o["optionId"].as_str())
        });
        let outcome = match option {
            Some(option) => json!({ "outcome": "selected", "optionId": option }),
            None => json!({ "outcome": "cancelled" }),
        };
        self.write(json!({ "jsonrpc": "2.0", "id": id, "result": { "outcome": outcome } }));
    }

    fn acp_call(&mut self, call: Call, method: &str, params: Value) {
        let id = self.next_call;
        self.next_call += 1;
        self.calls.insert(id, call);
        self.write(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
    }
}

// a skill or command picked from the menu arrives as `$name`, and opencode runs commands written as `/name`
fn command_text(text: &str, skills: &[Skill]) -> String {
    match crate::skills::mentions(text, skills).first() {
        Some(&(0, end, skill)) => format!("/{}{}", skill.name, &text[end..]),
        _ => text.to_owned(),
    }
}

// images go inline, and other files as links opencode's read tool can open
fn prompt_blocks(text: &str, attachments: &[String]) -> Value {
    let mut text = text.to_owned();
    let mut blocks = Vec::new();
    for path in attachments {
        if !is_image(path) {
            blocks.push(json!({ "type": "resource_link", "uri": format!("file://{path}"), "name": file_name(path) }));
            continue;
        }
        match fs::read(path) {
            Ok(bytes) => blocks.push(json!({
                "type": "image",
                "mimeType": media_type(path),
                "data": base64::engine::general_purpose::STANDARD.encode(bytes),
            })),
            Err(err) => text.push_str(&format!("\n\n(Couldn't attach {path}: {err})")),
        }
    }
    blocks.insert(0, json!({ "type": "text", "text": text }));
    Value::Array(blocks)
}

// a turn that ended for a reason other than finishing or being stopped says why
fn refusal(reason: &Value) -> Option<String> {
    match reason.as_str()? {
        "max_tokens" => Some("OpenCode stopped because the reply hit its token limit.".to_owned()),
        "max_turn_requests" => {
            Some("OpenCode stopped after too many steps in one turn.".to_owned())
        }
        "refusal" => Some("The model refused to continue.".to_owned()),
        _ => None,
    }
}

fn command_of(call: &Value) -> String {
    call["rawInput"]["command"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

fn locations(call: &Value) -> Vec<String> {
    call["locations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["path"].as_str().map(str::to_owned))
        .collect()
}

// what a tool is looking at, from its input or else the paths it reported
fn detail_of(call: &Value) -> String {
    let input = &call["rawInput"];
    ["path", "filePath", "pattern", "query", "url", "description"]
        .iter()
        .find_map(|key| input[key].as_str().filter(|s| !s.is_empty()))
        .map(str::to_owned)
        .or_else(|| locations(call).into_iter().next())
        .unwrap_or_default()
}

fn content_text(content: &Value) -> Option<String> {
    let texts: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|c| c["type"] == "content")
        .filter_map(|c| c["content"]["text"].as_str())
        .collect();
    (!texts.is_empty()).then(|| texts.join("\n"))
}

// opencode's own patch reads best when it sent one, and the before and after text stands in otherwise
fn file_diffs(update: &Value) -> Vec<FileChange> {
    let patches: Vec<FileChange> = update["rawOutput"]["metadata"]["files"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some(FileChange {
                path: f["file"].as_str()?.to_owned(),
                diff: hunks(f["patch"].as_str().unwrap_or_default()),
            })
        })
        .collect();
    if !patches.is_empty() {
        return patches;
    }
    update["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["type"] == "diff")
        .filter_map(|c| {
            let old = c["oldText"].as_str().unwrap_or_default();
            let new = c["newText"].as_str().unwrap_or_default();
            let mut diff = String::from("@@\n");
            for line in old.lines() {
                diff.push_str(&format!("-{line}\n"));
            }
            for line in new.lines() {
                diff.push_str(&format!("+{line}\n"));
            }
            Some(FileChange {
                path: c["path"].as_str()?.to_owned(),
                diff,
            })
        })
        .collect()
}

// the patch's index and file header lines repeat the path the card already shows
fn hunks(patch: &str) -> String {
    match patch.find("\n@@") {
        Some(start) => patch[start + 1..].to_owned(),
        None => patch.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use mux_core::Provider;
    use mux_core::protocol::Decision;
    use tokio::sync::mpsc;

    use super::super::Process;
    use super::*;

    fn update(update: Value) -> Value {
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": "s", "update": update } })
    }

    #[test]
    fn rejected_settings_are_retried_before_a_prompt() {
        let mut agent = Agent::load(std::path::Path::new("/nonexistent"));
        agent.settings.provider = Provider::OpenCode;
        agent.settings.model = Some("chosen".into());
        agent.settings.plan = true;
        agent.session_id = Some("s".into());
        agent.thread_live = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.record("first", &[], false);
        agent.opencode_send("first".into(), Vec::new());
        let first: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(first["params"]["configId"], "model");
        assert!(rx.try_recv().is_err());
        agent.handle(json!({ "id": first["id"], "error": { "message": "try again" } }));
        assert!(!agent.busy);
        assert!(agent.applied.model.is_none());
        assert!(rx.try_recv().is_err());

        agent.record("second", &[], false);
        agent.opencode_send("second".into(), Vec::new());
        let retry: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(retry["params"]["configId"], "model");
        agent.handle(json!({ "id": retry["id"], "result": {} }));
        let mode: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(mode["params"]["configId"], "mode");
        assert!(rx.try_recv().is_err());
        agent.handle(json!({ "id": mode["id"], "result": {} }));
        let prompt: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(prompt["method"], "session/prompt");
        assert_eq!(prompt["params"]["prompt"][0]["text"], "second");
    }

    #[test]
    fn stopping_during_configuration_discards_the_prompt() {
        let mut agent = Agent::load(std::path::Path::new("/nonexistent"));
        agent.settings.provider = Provider::OpenCode;
        agent.settings.plan = true;
        agent.session_id = Some("s".into());
        agent.thread_live = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.record("go", &[], false);
        agent.opencode_send("go".into(), Vec::new());
        let config: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        agent.stop();
        agent.handle(json!({ "id": config["id"], "result": {} }));
        assert!(!agent.busy);
        assert!(agent.applied.plan);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_new_prompt_waits_for_cancelled_configuration_to_settle() {
        let mut agent = Agent::load(std::path::Path::new("/nonexistent"));
        agent.settings.provider = Provider::OpenCode;
        agent.settings.plan = true;
        agent.session_id = Some("s".into());
        agent.thread_live = true;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.record("first", &[], false);
        agent.opencode_send("first".into(), Vec::new());
        let config: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        agent.stop();
        agent.settings.plan = false;
        agent.record("second", &[], false);
        agent.opencode_send("second".into(), Vec::new());
        assert!(rx.try_recv().is_err());
        agent.handle(json!({ "id": config["id"], "result": {} }));
        let restore: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(restore["params"]["value"], "build");
        assert!(rx.try_recv().is_err());
        agent.handle(json!({ "id": restore["id"], "result": {} }));
        let prompt: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(prompt["params"]["prompt"][0]["text"], "second");
    }

    #[test]
    fn maps_an_acp_turn_onto_the_transcript() {
        let mut agent = Agent::load(std::path::Path::new("/nonexistent"));
        agent.settings.provider = Provider::OpenCode;
        agent.settings.access = Access::Supervised;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.cwd = Some("/p".into());
        agent.opencode_initialize();
        agent.handle(json!({ "jsonrpc": "2.0", "id": 0, "result": {} }));
        agent.handle(
            json!({ "jsonrpc": "2.0", "id": 1, "result": { "sessionId": "s", "configOptions": [
            { "id": "mode", "currentValue": "build" }
        ] } }),
        );
        agent.record("hi", &[], false);
        agent.transmit("hi".into(), Vec::new(), false);

        agent.handle(update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "Need to " } })));
        agent.handle(update(json!({ "sessionUpdate": "agent_thought_chunk", "content": { "type": "text", "text": "run it." } })));
        agent.handle(update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "Run" } })));
        agent.handle(update(json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "ning it." } })));
        agent.handle(update(json!({ "sessionUpdate": "tool_call", "toolCallId": "c", "kind": "execute", "status": "pending", "rawInput": {} })));
        // opencode numbers its own requests from 0 too, which must not be mistaken for replies
        agent.handle(json!({ "jsonrpc": "2.0", "id": 0, "method": "session/request_permission", "params": {
            "toolCall": { "toolCallId": "c", "kind": "execute", "rawInput": { "command": "ls" } },
            "options": [{ "optionId": "once", "kind": "allow_once" }, { "optionId": "reject", "kind": "reject_once" }]
        } }));
        agent.decide(1, Decision::Allow);
        agent.handle(update(json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c", "status": "completed",
            "rawInput": { "command": "ls" }, "content": [{ "type": "content", "content": { "type": "text", "text": "a.txt" } }],
            "rawOutput": { "metadata": { "exit": 0 } } })));
        agent.handle(update(
            json!({ "sessionUpdate": "usage_update", "used": 900, "size": 1000 }),
        ));
        agent.handle(json!({ "jsonrpc": "2.0", "id": 2, "result": { "stopReason": "end_turn" } }));

        assert!(
            matches!(&agent.items[1], AgentItem::Reasoning { text, ended: Some(_), .. } if text == "Need to run it.")
        );
        assert!(matches!(&agent.items[2], AgentItem::Assistant { text } if text == "Running it."));
        assert!(
            matches!(&agent.items[3], AgentItem::Command { command, output, status: ItemStatus::Done, .. }
            if command == "ls" && output == "a.txt")
        );
        assert!(matches!(
            &agent.items[4],
            AgentItem::Approval {
                decision: Some(Decision::Allow),
                ..
            }
        ));
        assert_eq!(
            agent.context,
            Some(ContextUsage {
                used: 900,
                window: 1000
            })
        );
        assert!(!agent.busy);
        let sent: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|line| serde_json::from_str(&line).unwrap())
            .collect();
        assert!(
            sent.iter().any(
                |m| m["method"] == "session/prompt" && m["params"]["prompt"][0]["text"] == "hi"
            )
        );
        assert!(
            sent.iter()
                .any(|m| m["id"] == 0 && m["result"]["outcome"]["optionId"] == "once")
        );
    }

    #[test]
    fn picked_commands_run_as_slash_commands() {
        let skills = [Skill {
            name: "review".into(),
            description: String::new(),
            path: String::new(),
            scope: "OpenCode".into(),
        }];
        assert_eq!(
            command_text("$review the branch", &skills),
            "/review the branch"
        );
        assert_eq!(command_text("please $review", &skills), "please $review");
    }
}
