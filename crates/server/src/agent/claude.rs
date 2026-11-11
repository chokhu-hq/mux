use super::*;

impl Agent {
    // starts claude on a new conversation after the old one was lost, sending again what the old one never read
    pub(super) fn start_over(&mut self) -> bool {
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
    pub(super) fn reconfigure_claude(&mut self) -> bool {
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

    pub(super) fn claude_control(&mut self, subtype: &str, mut request: Value) {
        request["subtype"] = json!(subtype);
        self.write(json!({
            "type": "control_request",
            "request_id": format!("{CONFIG_REQUEST}{}", self.next_call),
            "request": request,
        }));
        self.next_call += 1;
    }

    pub(super) fn handle_claude(&mut self, msg: &Value) {
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
                // claude says how each server started, without a reason when one failed
                for server in msg["mcp_servers"].as_array().into_iter().flatten() {
                    let state = match server["status"].as_str() {
                        Some("connected") => McpState::Connected,
                        Some("failed") => McpState::Failed(String::new()),
                        Some("pending") => McpState::Starting,
                        _ => continue,
                    };
                    self.mcp_started(server["name"].as_str().unwrap_or_default(), state);
                }
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
                            if msg["error"] == "rate_limit" {
                                self.usage_limit(text, None);
                                continue;
                            }
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
                    if let Some(reset) = self.limit_reset.take() {
                        self.pause_until(reset, message);
                    } else if self.items.last().is_some_and(|item| matches!(item,
                        AgentItem::UsageLimit { message: previous, .. } if previous.trim() == message.trim()
                    )) {
                        self.usage_limit(message, None);
                    } else if let Some(last) = self.items.len().checked_sub(1).filter(|&i| {
                        matches!(&self.items[i], AgentItem::Assistant { text } if text.trim() == message.trim())
                    }) {
                        // claude often says the error as a reply too, which becomes the error rather than showing twice
                        self.items[last] = AgentItem::Error { message };
                        self.changed.insert(last);
                    } else {
                        self.push(AgentItem::Error { message });
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
    pub(super) fn claude_echo(&mut self, msg: &Value) {
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
        // a message of only pictures has no text to find, so it counts as echoed along with the rest
        while let Some(next) = self.claude_waiting.front() {
            if !next.trim().is_empty() && !text.contains(next.trim()) {
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

    pub(super) fn claude_tool(&mut self, block: &Value) {
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
                images: Vec::new(),
            },
        };
        let index = self.push(entry);
        self.by_codex_id.insert(id, index);
    }

    pub(super) fn claude_tool_result(&mut self, msg: &Value, block: &Value) {
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
        let (text, pictures) = if matches!(self.items.get(index), Some(AgentItem::Tool { .. })) {
            // claude can name its saved copy and also send the same picture inline, which would show twice
            let (text, mut pictures) = crate::mcp::image_refs(&text);
            if pictures.is_empty() {
                pictures = crate::mcp::save_images(&block["content"], self.session);
            }
            (text, pictures)
        } else {
            (text, Vec::new())
        };
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
            }) => {
                *output = text;
                trim_front(output);
                *s = status;
            }
            Some(AgentItem::Tool {
                output,
                status: s,
                images,
                ..
            }) => {
                *output = text;
                trim_front(output);
                *s = status;
                *images = pictures;
            }
            Some(AgentItem::FileChange { status: s, .. }) => *s = status,
            _ => return,
        }
        self.changed.insert(index);
    }

    pub(super) fn claude_request(&mut self, msg: &Value) {
        let request = &msg["request"];
        let id = msg["request_id"].clone();
        if request["subtype"] == "elicitation" {
            let server = request["mcp_server_name"].as_str().unwrap_or_default();
            self.mcp_form(id, server, request);
            return;
        }
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
            _ if request["mcp_server"]["name"].is_string() => {
                let server = request["mcp_server"]["name"].as_str().unwrap_or_default();
                let shown = request["display_name"].as_str().unwrap_or(tool);
                (
                    format!("Allow {server} to use {shown}?"),
                    tool_detail(&input),
                )
            }
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
}

// sent on every turn, so a change in the picker applies to the very next message
// claude gets images inline, and other files as paths its read tool can open
pub(super) fn claude_content(text: &str, attachments: &[String], skills: &[Skill]) -> Value {
    let (images, files): (Vec<&String>, Vec<&String>) =
        attachments.iter().partition(|p| is_image(p));
    // a mentioned skill needs its own block to start with `/name`, and the rest goes ahead of it
    let mut blocks = crate::skills::claude_blocks(text, skills);
    let mut text = blocks.pop().unwrap_or_default();
    text.push_str(&attached(&files));
    let mut text = text.trim_start().to_owned();
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

// everything claude sent with a request counts toward the window, cached or not
pub(super) fn claude_context(usage: &Value) -> Option<u64> {
    let input = usage["input_tokens"].as_u64()?;
    Some(
        input
            + usage["cache_creation_input_tokens"].as_u64().unwrap_or(0)
            + usage["cache_read_input_tokens"].as_u64().unwrap_or(0)
            + usage["output_tokens"].as_u64().unwrap_or(0),
    )
}

pub(super) fn claude_args(
    settings: &AgentSettings,
    session: Option<&str>,
    at: Option<&str>,
) -> Vec<String> {
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

pub(crate) fn forget_parent_claude(command: &mut Command) {
    for key in PARENT_CLAUDE {
        command.env_remove(key);
    }
}

// plan mode stands in for the access level until the plan is accepted
pub(super) fn permission_mode(settings: &AgentSettings) -> &'static str {
    match settings.access {
        _ if settings.plan => "plan",
        Access::Supervised => "manual",
        Access::AutoEdits => "acceptEdits",
        Access::Auto => "auto",
        Access::Full => "bypassPermissions",
    }
}

// what's wrong in a few words, from the status and kind claude gives a retry
pub(super) fn claude_retry_reason(msg: &Value) -> String {
    match (msg["error_status"].as_u64(), msg["error"].as_str()) {
        (Some(429), _) | (_, Some("rate_limit")) => "Claude is rate limited".to_owned(),
        (Some(529), _) | (_, Some("overloaded")) => "Claude's servers are busy".to_owned(),
        (Some(status), _) => format!("Claude returned an error ({status})"),
        (None, _) => "Couldn't reach Claude".to_owned(),
    }
}

// whether a result says claude couldn't find the conversation it was told to resume
pub(super) fn lost(msg: &Value) -> bool {
    msg["errors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|e| e.starts_with("No conversation found"))
}

// why a turn failed in words, or none when it didn't or when it only ended because it was stopped
pub(super) fn claude_failure(msg: &Value, stopping: bool) -> Option<String> {
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
pub(super) fn claude_answer(decision: Decision, input: Value, suggestions: &Value) -> Value {
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
pub(super) fn edit_diff(tool: &str, input: &Value) -> String {
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
