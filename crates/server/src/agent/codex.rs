use super::*;

impl Agent {
    // the item codex's notification is about, for adding streamed text to
    pub(super) fn codex_index(&self, params: &Value) -> Option<usize> {
        self.by_codex_id.get(params["itemId"].as_str()?).copied()
    }

    pub(super) fn response(&mut self, id: Value, msg: &Value) {
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
                None => {
                    self.turn_id = msg["result"]["turn"]["id"].as_str().map(str::to_owned);
                    if self.stop_requested {
                        self.stop();
                    }
                }
            },
            Call::Steer(text, attachments) => {
                if error.is_some() {
                    self.start_turn(text, attachments);
                }
            }
            Call::Interrupt | Call::Configure(..) => {}
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

    pub(super) fn request(&mut self, method: &str, id: Value, params: &Value) {
        if method == "item/tool/requestUserInput" {
            self.codex_question(id, params);
            return;
        }
        let server = params["serverName"].as_str().unwrap_or_default();
        if method == "mcpServer/elicitation/request"
            && params["_meta"]["codex_approval_kind"] != "mcp_tool_call"
        {
            self.mcp_form(id, server, params);
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
            "mcpServer/elicitation/request" => {
                let meta = &params["_meta"];
                let tool = meta["tool_name"].as_str().unwrap_or_default();
                let shown = meta["tool_params_display"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|p| {
                        let name = p["display_name"].as_str().or(p["name"].as_str());
                        let value = match &p["value"] {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        };
                        format!("{}: {value}", name.unwrap_or_default())
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let title = format!("Allow {server} to use {tool}?");
                self.approve(id, title, shown, json!({ "mcp": meta["persist"] }));
                return;
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
        self.approve(id, title.to_owned(), detail, Value::Null);
    }

    pub(super) fn approve(&mut self, request: Value, title: String, detail: String, input: Value) {
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
                request,
                index,
                input,
                suggestions: Value::Null,
            },
        );
    }

    pub(super) fn notification(&mut self, method: &str, params: &Value) {
        if self.codex_child(method, params) {
            return;
        }
        match method {
            "mcpServer/startupStatus/updated" => {
                let state = match params["status"].as_str() {
                    Some("ready") => McpState::Connected,
                    Some("starting") => McpState::Starting,
                    Some("failed") => {
                        McpState::Failed(params["error"].as_str().unwrap_or_default().to_owned())
                    }
                    _ => return,
                };
                self.mcp_started(params["name"].as_str().unwrap_or_default(), state);
            }
            "turn/started" => {
                self.turn_id = params["turn"]["id"].as_str().map(str::to_owned);
                if let Some(turn) = self.turn_id.clone() {
                    self.mark_turn(&turn);
                }
                self.set_busy(true);
                if self.stop_requested {
                    self.stop();
                }
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
                    if turn["error"]["codexErrorInfo"] == "usageLimitExceeded" {
                        match usage::blocked_until(&self.windows) {
                            Some(reset) => self.pause_until(reset, message.to_owned()),
                            None => self.usage_limit(message.to_owned(), None),
                        }
                    } else {
                        self.push(AgentItem::Error {
                            message: message.to_owned(),
                        });
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

    pub(super) fn item_started(&mut self, item: &Value) {
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
            // named the way claude names them, so both show the same
            Some("mcpToolCall") => AgentItem::Tool {
                name: format!(
                    "mcp__{}__{}",
                    item["server"].as_str().unwrap_or_default(),
                    item["tool"].as_str().unwrap_or_default()
                ),
                detail: tool_detail(&item["arguments"]),
                output: String::new(),
                status: ItemStatus::Running,
                images: Vec::new(),
            },
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

    pub(super) fn item_completed(&mut self, item: &Value) {
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
            Some(AgentItem::Tool {
                output,
                status: s,
                images,
                ..
            }) => {
                let content = &item["result"]["content"];
                *output = match item["error"]["message"].as_str().or(item["error"].as_str()) {
                    Some(error) => error.to_owned(),
                    None => result_text(content),
                };
                trim_front(output);
                *s = status;
                *images = crate::mcp::save_images(content, self.session);
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

    pub(super) fn open_thread(&mut self) {
        let mut params = json!({ "approvalPolicy": "on-request", "sandbox": "workspace-write" });
        if let Some(cwd) = &self.cwd {
            params["cwd"] = json!(cwd);
        }
        if let Some(servers) = crate::mcp::codex(&self.mcp) {
            params["config"] = json!({ "mcp_servers": servers });
        }
        match self.thread_id.clone() {
            Some(thread) => {
                params["threadId"] = json!(thread);
                self.call(Call::ResumeThread, "thread/resume", params);
            }
            None => self.call(Call::StartThread, "thread/start", params),
        }
    }

    pub(super) fn start_turn(&mut self, text: String, attachments: Vec<String>) {
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
}

// codex reads images itself and gets other files as mentions it can open
// codex is handed each skill a message mentions by its path, alongside the text that names it
pub(super) fn add_skills(params: &mut Value, text: &str, skills: &[Skill]) {
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

pub(super) fn turn_params(
    thread: &str,
    text: &str,
    attachments: &[String],
    settings: &AgentSettings,
) -> Value {
    let (images, files): (Vec<&String>, Vec<&String>) =
        attachments.iter().partition(|p| is_image(p));
    // codex's mention input is for its own apps and plugins, and passes over a file or folder in silence
    let text = format!("{text}{}", attached(&files));
    let mut input = vec![json!({ "type": "text", "text": text.trim_start() })];
    for path in images {
        input.push(json!({ "type": "localImage", "path": path }));
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

// codex says "Reconnecting... 2/5 (stream disconnected)", with the attempt and why
pub(super) fn codex_retry(message: &str) -> (String, u32, Option<u32>) {
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

pub(super) fn file_changes(item: &Value) -> Vec<FileChange> {
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
