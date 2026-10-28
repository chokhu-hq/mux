use mux_core::protocol::{AgentItem, ItemStatus, SubagentStep};
use serde_json::Value;

use super::{Agent, item_status, result_text, tool_detail};

// the parts of a subagent card that change while it runs
struct Parts<'a> {
    background: bool,
    status: &'a mut ItemStatus,
    activity: &'a mut String,
    steps: &'a mut Vec<SubagentStep>,
    result: &'a mut String,
    tokens: &'a mut u64,
}

impl Agent {
    fn subagent(&mut self, index: usize) -> Option<Parts<'_>> {
        self.changed.insert(index);
        match self.items.get_mut(index) {
            Some(AgentItem::Subagent {
                background,
                status,
                activity,
                steps,
                result,
                tokens,
                ..
            }) => Some(Parts {
                background: *background,
                status,
                activity,
                steps,
                result,
                tokens,
            }),
            _ => None,
        }
    }

    // claude's Agent tool, called Task in older versions, starts a subagent
    pub(super) fn claude_subagent(&mut self, id: String, input: &Value) {
        let text = |key: &str| input[key].as_str().unwrap_or_default().to_owned();
        let index = self.push(AgentItem::Subagent {
            task: text("description"),
            kind: text("subagent_type"),
            prompt: text("prompt"),
            background: input["run_in_background"] == true,
            status: ItemStatus::Running,
            activity: String::new(),
            steps: Vec::new(),
            result: String::new(),
            tokens: 0,
        });
        self.by_codex_id.insert(id, index);
    }

    // everything a subagent says arrives tagged with the tool call that started it
    pub(super) fn claude_subagent_message(&mut self, parent: &str, msg: &Value) {
        let Some(&index) = self.by_codex_id.get(parent) else {
            return;
        };
        let blocks = msg["message"]["content"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for block in &blocks {
            match (msg["type"].as_str(), block["type"].as_str()) {
                (Some("assistant"), Some("tool_use")) => {
                    let name = block["name"].as_str().unwrap_or_default().to_owned();
                    let input = &block["input"];
                    let detail = match input["command"].as_str() {
                        Some(command) => command.lines().next().unwrap_or_default().to_owned(),
                        None => tool_detail(input),
                    };
                    let Some(parts) = self.subagent(index) else {
                        return;
                    };
                    *parts.activity = format!("{name} {detail}").trim().to_owned();
                    parts.steps.push(SubagentStep {
                        name,
                        detail,
                        status: ItemStatus::Running,
                    });
                    let step = parts.steps.len() - 1;
                    if let Some(id) = block["id"].as_str() {
                        self.sub_steps.insert(id.to_owned(), (index, step));
                    }
                }
                (Some("assistant"), Some("text")) => {
                    let text = block["text"].as_str().unwrap_or_default().trim();
                    if !text.is_empty()
                        && let Some(parts) = self.subagent(index)
                    {
                        *parts.result = text.to_owned();
                    }
                }
                (Some("user"), Some("tool_result")) => {
                    let failed = block["is_error"] == true;
                    let Some(&(index, step)) = block["tool_use_id"]
                        .as_str()
                        .and_then(|id| self.sub_steps.get(id))
                    else {
                        continue;
                    };
                    if let Some(parts) = self.subagent(index)
                        && let Some(step) = parts.steps.get_mut(step)
                    {
                        step.status = if failed {
                            ItemStatus::Failed
                        } else {
                            ItemStatus::Done
                        };
                    }
                }
                _ => {}
            }
        }
    }

    // claude reports a subagent's start, progress and end as task events, keyed by a task id of their own
    pub(super) fn claude_task(&mut self, msg: &Value) {
        let task = msg["task_id"].as_str().unwrap_or_default().to_owned();
        let index = match msg["tool_use_id"].as_str() {
            Some(tool) => self.by_codex_id.get(tool).copied(),
            None => self.tasks.get(&task).copied(),
        };
        let Some(index) = index else {
            return;
        };
        match msg["subtype"].as_str() {
            Some("task_started") => {
                self.tasks.insert(task, index);
            }
            Some("task_progress") => {
                let description = msg["description"].as_str().unwrap_or_default().to_owned();
                let tokens = msg["usage"]["total_tokens"].as_u64();
                let Some(parts) = self.subagent(index) else {
                    return;
                };
                if let Some(tokens) = tokens {
                    *parts.tokens = tokens;
                }
                // the first progress note just repeats the task, so only later ones say what it's doing
                if parts.activity.is_empty() || !parts.steps.is_empty() {
                    *parts.activity = description;
                }
            }
            Some("task_updated") => {
                let status = msg["patch"]["status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned();
                self.finish_subagent(index, &status);
            }
            Some("task_notification") => {
                let status = msg["status"].as_str().unwrap_or_default().to_owned();
                self.finish_subagent(index, &status);
            }
            _ => {}
        }
    }

    fn finish_subagent(&mut self, index: usize, status: &str) {
        let done = match status {
            "completed" => ItemStatus::Done,
            "failed" | "killed" | "stopped" | "error" | "cancelled" => ItemStatus::Failed,
            _ => return,
        };
        if let Some(parts) = self.subagent(index) {
            *parts.status = done;
            parts.activity.clear();
        }
    }

    // the Agent tool's result is the subagent's final report, except a background one's, which only says it started
    pub(super) fn claude_subagent_result(&mut self, index: usize, msg: &Value, block: &Value) {
        let failed = block["is_error"] == true;
        let report = msg["tool_use_result"]["content"]
            .as_array()
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|p| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| result_text(&block["content"]));
        let Some(parts) = self.subagent(index) else {
            return;
        };
        if parts.background && !failed {
            return;
        }
        *parts.result = report;
        *parts.status = if failed {
            ItemStatus::Failed
        } else {
            ItemStatus::Done
        };
        parts.activity.clear();
    }

    // codex's spawnAgent call names the new agent's thread, and later calls like wait report on it
    pub(super) fn codex_collab(&mut self, item: &Value) {
        let id = item["id"].as_str().unwrap_or_default().to_owned();
        if item["tool"] == "spawnAgent" {
            let index = match self.by_codex_id.get(&id) {
                Some(&index) => index,
                None => {
                    let prompt = item["prompt"].as_str().unwrap_or_default().to_owned();
                    let mut task: String = prompt
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(80)
                        .collect();
                    if task.is_empty() {
                        task = "Subagent".to_owned();
                    }
                    let index = self.push(AgentItem::Subagent {
                        task,
                        kind: item["model"].as_str().unwrap_or_default().to_owned(),
                        prompt,
                        background: false,
                        status: ItemStatus::Running,
                        activity: String::new(),
                        steps: Vec::new(),
                        result: String::new(),
                        tokens: 0,
                    });
                    self.by_codex_id.insert(id, index);
                    index
                }
            };
            for thread in item["receiverThreadIds"].as_array().into_iter().flatten() {
                if let Some(thread) = thread.as_str() {
                    self.child_threads.insert(thread.to_owned(), index);
                }
            }
        }
        for (thread, state) in item["agentsStates"].as_object().into_iter().flatten() {
            let Some(&index) = self.child_threads.get(thread) else {
                continue;
            };
            let message = state["message"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_owned();
            let status = match state["status"].as_str() {
                Some("completed" | "shutdown") => Some(ItemStatus::Done),
                Some("errored" | "interrupted" | "notFound") => Some(ItemStatus::Failed),
                _ => None,
            };
            let Some(parts) = self.subagent(index) else {
                continue;
            };
            if !message.is_empty() {
                *parts.result = message;
            }
            if let Some(status) = status {
                *parts.status = status;
                parts.activity.clear();
            }
        }
    }

    // a notification from another thread belongs to a subagent and never touches the main turn
    pub(super) fn codex_child(&mut self, method: &str, params: &Value) -> bool {
        let Some(thread) = params["threadId"].as_str() else {
            return false;
        };
        if self.thread_id.as_deref().is_none_or(|main| main == thread) {
            return false;
        }
        let Some(&index) = self.child_threads.get(thread) else {
            return true;
        };
        let item = &params["item"];
        let id = item["id"].as_str().unwrap_or_default().to_owned();
        match method {
            "item/started" => {
                let (name, detail) = match item["type"].as_str() {
                    Some("commandExecution") => (
                        "Bash",
                        item["command"].as_str().unwrap_or_default().to_owned(),
                    ),
                    Some("fileChange") => (
                        "Edit",
                        item["changes"][0]["path"]
                            .as_str()
                            .unwrap_or_default()
                            .to_owned(),
                    ),
                    Some("mcpToolCall") => {
                        ("Tool", item["tool"].as_str().unwrap_or_default().to_owned())
                    }
                    Some("webSearch") => (
                        "WebSearch",
                        item["query"].as_str().unwrap_or_default().to_owned(),
                    ),
                    _ => return true,
                };
                let Some(parts) = self.subagent(index) else {
                    return true;
                };
                *parts.activity = format!("{name} {detail}");
                parts.steps.push(SubagentStep {
                    name: name.to_owned(),
                    detail,
                    status: ItemStatus::Running,
                });
                let step = parts.steps.len() - 1;
                self.sub_steps.insert(id, (index, step));
            }
            "item/completed" => {
                if item["type"] == "agentMessage" {
                    let text = item["text"].as_str().unwrap_or_default().trim().to_owned();
                    if !text.is_empty()
                        && let Some(parts) = self.subagent(index)
                    {
                        *parts.result = text;
                    }
                } else if let Some(&(index, step)) = self.sub_steps.get(&id) {
                    let status = match item_status(item) {
                        ItemStatus::Running => ItemStatus::Done,
                        status => status,
                    };
                    if let Some(parts) = self.subagent(index)
                        && let Some(step) = parts.steps.get_mut(step)
                    {
                        step.status = status;
                    }
                }
            }
            "thread/tokenUsage/updated" => {
                let total = params["tokenUsage"]["total"]["totalTokens"].as_u64();
                if let Some(total) = total
                    && let Some(parts) = self.subagent(index)
                {
                    *parts.tokens = total;
                }
            }
            _ => {}
        }
        true
    }

    // nothing can still be running once the cli that ran it has gone
    pub(super) fn fail_subagents(&mut self) {
        for (index, item) in self.items.iter_mut().enumerate() {
            if let AgentItem::Subagent {
                status, activity, ..
            } = item
                && *status == ItemStatus::Running
            {
                *status = ItemStatus::Failed;
                activity.clear();
                self.changed.insert(index);
            }
        }
    }
}
