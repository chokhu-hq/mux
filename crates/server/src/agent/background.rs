use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mux_core::protocol::{AgentItem, Background, BackgroundState, ItemStatus};
use serde_json::{Value, json};

use super::Agent;
use crate::Event;

// how much of a background command's output is kept and shown
const TAIL_BYTES: u64 = 16 * 1024;

// a command the agent left running, tied to its row in the chat
pub(super) struct Tracked {
    id: String,
    label: String,
    started: u64,
    state: BackgroundState,
    // the command's row in the transcript, which shows its output as it grows
    index: Option<usize>,
    // claude writes the output to a file, which mux follows until the command ends
    file: Option<PathBuf>,
    stoppable: bool,
    done: Arc<AtomicBool>,
}

impl Agent {
    fn tracked(&mut self, id: &str) -> &mut Tracked {
        if let Some(at) = self.background.iter().position(|t| t.id == id) {
            return &mut self.background[at];
        }
        self.background.push(Tracked {
            id: id.to_owned(),
            label: String::new(),
            started: now(),
            state: BackgroundState::Running,
            index: None,
            file: None,
            stoppable: false,
            done: Arc::new(AtomicBool::new(false)),
        });
        self.background_changed = true;
        self.background.last_mut().expect("just pushed")
    }

    // claude's task events for a command it moved to the background, rather than for a subagent
    pub(super) fn claude_background_task(&mut self, msg: &Value) -> bool {
        let task = msg["task_id"].as_str().unwrap_or_default().to_owned();
        let known = self.background.iter().any(|t| t.id == task);
        if !known && msg["task_type"] != "local_bash" {
            return false;
        }
        match msg["subtype"].as_str() {
            Some("task_started") => {
                let index = msg["tool_use_id"]
                    .as_str()
                    .and_then(|tool| self.by_codex_id.get(tool).copied());
                let label = msg["description"].as_str().unwrap_or_default().to_owned();
                let tracked = self.tracked(&task);
                tracked.label = label;
                tracked.stoppable = true;
                if index.is_some() {
                    tracked.index = index;
                }
            }
            Some("task_updated") => {
                let status = msg["patch"]["status"].as_str().unwrap_or_default();
                self.end_background(&task, status, None);
            }
            Some("task_notification") => {
                let status = msg["status"].as_str().unwrap_or_default().to_owned();
                let code = exit_code(msg["summary"].as_str().unwrap_or_default());
                if let Some(file) = msg["output_file"].as_str() {
                    self.tracked(&task).file = Some(file.into());
                }
                self.end_background(&task, &status, code);
            }
            _ => {}
        }
        true
    }

    // claude answers a background command at once with where its output goes, which mux then follows
    pub(super) fn claude_backgrounded(&mut self, index: usize, result: &str) -> bool {
        let Some((task, file)) = background_result(result) else {
            return false;
        };
        let command = match self.items.get(index) {
            Some(AgentItem::Command { command, .. }) => command.clone(),
            _ => String::new(),
        };
        let tracked = self.tracked(&task);
        tracked.index = Some(index);
        tracked.file = Some(file.clone());
        tracked.stoppable = true;
        if tracked.label.is_empty() {
            tracked.label = command;
        }
        let done = tracked.done.clone();
        self.follow(task, file, done);
        self.background_changed = true;
        true
    }

    // reads the output file about once a second until the command ends, so its row and the strip stay current
    fn follow(&self, task: String, file: PathBuf, done: Arc<AtomicBool>) {
        let Some(events) = self.events.clone() else {
            return;
        };
        let (session, generation) = (self.session, self.generation);
        tokio::spawn(async move {
            let mut last = String::new();
            while !done.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let output = mux_core::logging::tail(&file, TAIL_BYTES);
                if output != last {
                    last = output.clone();
                    let event = Event::BackgroundOutput {
                        session,
                        generation,
                        task: task.clone(),
                        output,
                    };
                    if events.send(event).is_err() {
                        return;
                    }
                }
            }
        });
    }

    pub fn background_output(&mut self, task: &str, output: String) {
        let Some(index) = self
            .background
            .iter()
            .find(|t| t.id == task && t.state == BackgroundState::Running)
            .and_then(|t| t.index)
        else {
            return;
        };
        if let Some(AgentItem::Command { output: shown, .. }) = self.items.get_mut(index) {
            *shown = output;
            self.changed.insert(index);
            self.background_changed = true;
        }
    }

    fn end_background(&mut self, task: &str, status: &str, code: Option<i32>) {
        let state = match status {
            "completed" => BackgroundState::Exited(code),
            "failed" | "error" => BackgroundState::Exited(code.or(Some(1))),
            "killed" | "stopped" | "cancelled" => BackgroundState::Stopped,
            _ => return,
        };
        let Some(tracked) = self.background.iter_mut().find(|t| t.id == task) else {
            return;
        };
        // the final notification can repeat an update that already ended it, and only the first one counts
        if tracked.state != BackgroundState::Running {
            if let (BackgroundState::Exited(None), BackgroundState::Exited(Some(_))) =
                (tracked.state, state)
            {
                tracked.state = state;
            } else {
                return;
            }
        } else {
            tracked.state = state;
        }
        tracked.done.store(true, Ordering::Relaxed);
        let (index, file) = (tracked.index, tracked.file.clone());
        self.background_changed = true;
        let Some(index) = index else {
            return;
        };
        let final_output = file.map(|f| mux_core::logging::tail(&f, TAIL_BYTES));
        if let Some(AgentItem::Command {
            output,
            exit_code,
            status,
            ..
        }) = self.items.get_mut(index)
        {
            if let Some(text) = final_output {
                *output = text;
            }
            let code = match state {
                BackgroundState::Exited(code) => code,
                _ => None,
            };
            *exit_code = code.or(*exit_code);
            *status = match state {
                BackgroundState::Exited(Some(0) | None) => ItemStatus::Done,
                _ => ItemStatus::Failed,
            };
            self.changed.insert(index);
        }
    }

    // codex keeps a command going past the end of its turn when it was left running, which makes it a background one
    pub(super) fn codex_backgrounded(&mut self) {
        let running: Vec<(String, usize)> = self
            .by_codex_id
            .iter()
            .filter(|(_, index)| {
                matches!(
                    self.items.get(**index),
                    Some(AgentItem::Command {
                        status: ItemStatus::Running,
                        ..
                    })
                )
            })
            .map(|(id, index)| (id.clone(), *index))
            .collect();
        for (id, index) in running {
            let command = match self.items.get_mut(index) {
                Some(AgentItem::Command {
                    command,
                    background,
                    ..
                }) => {
                    *background = true;
                    command.clone()
                }
                _ => continue,
            };
            self.changed.insert(index);
            let tracked = self.tracked(&id);
            tracked.index = Some(index);
            tracked.label = command;
        }
    }

    // a codex command that outlived its turn finishing, or more of its output arriving
    pub(super) fn codex_background_item(&mut self, index: usize) {
        let Some(at) = self.background.iter().position(|t| t.index == Some(index)) else {
            return;
        };
        self.background_changed = true;
        if self.background[at].state != BackgroundState::Running {
            return;
        }
        if let Some(AgentItem::Command {
            status, exit_code, ..
        }) = self.items.get(index)
            && *status != ItemStatus::Running
        {
            let tracked = &mut self.background[at];
            tracked.state = BackgroundState::Exited(*exit_code);
            tracked.done.store(true, Ordering::Relaxed);
        }
    }

    pub fn stop_background(&mut self, task: &str) {
        let stoppable = self
            .background
            .iter()
            .any(|t| t.id == task && t.stoppable && t.state == BackgroundState::Running);
        if !stoppable {
            return;
        }
        self.write(json!({
            "type": "control_request",
            "request_id": format!("mux-stop-task-{}", self.next_call),
            "request": { "subtype": "stop_task", "task_id": task }
        }));
        self.next_call += 1;
    }

    // finished commands stay listed until the next message, so a quick one isn't missed
    pub(super) fn clear_finished_background(&mut self) {
        let before = self.background.len();
        self.background
            .retain(|t| t.state == BackgroundState::Running);
        if self.background.len() != before {
            self.background_changed = true;
        }
    }

    // the agent going away takes whatever it left running with it
    pub(super) fn stop_all_background(&mut self) {
        for tracked in &mut self.background {
            if tracked.state == BackgroundState::Running {
                tracked.state = BackgroundState::Stopped;
                tracked.done.store(true, Ordering::Relaxed);
                self.background_changed = true;
                if let Some(index) = tracked.index
                    && let Some(AgentItem::Command { status, .. }) = self.items.get_mut(index)
                    && *status == ItemStatus::Running
                {
                    *status = ItemStatus::Failed;
                    self.changed.insert(index);
                }
            }
        }
    }

    pub fn background(&self) -> Vec<Background> {
        self.background
            .iter()
            .map(|t| {
                let (command, output) = match t.index.and_then(|i| self.items.get(i)) {
                    Some(AgentItem::Command {
                        command, output, ..
                    }) => (command.clone(), output.clone()),
                    _ => (String::new(), String::new()),
                };
                Background {
                    id: t.id.clone(),
                    label: if t.label.is_empty() {
                        command.clone()
                    } else {
                        t.label.clone()
                    },
                    command,
                    started: t.started,
                    state: t.state,
                    output,
                    stoppable: t.stoppable && t.state == BackgroundState::Running,
                }
            })
            .collect()
    }

    pub fn take_background(&mut self) -> Option<Vec<Background>> {
        std::mem::take(&mut self.background_changed).then(|| self.background())
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// "Command running in background with ID: b8knmzt35. Output is being written to: /tmp/…/b8knmzt35.output. …"
fn background_result(text: &str) -> Option<(String, PathBuf)> {
    let rest = text.split_once("running in background with ID:")?.1;
    let (id, rest) = rest.trim_start().split_once('.')?;
    let path = rest.split_once("written to:")?.1.trim_start();
    // the path ends where the sentence does, at a full stop followed by a space or the end
    let end = path.find(". ").unwrap_or(path.trim_end_matches('.').len());
    Some((id.trim().to_owned(), PathBuf::from(&path[..end])))
}

// "Background command "…" completed (exit code 0)"
fn exit_code(summary: &str) -> Option<i32> {
    let rest = summary.rsplit_once("exit code ")?.1;
    rest.trim_end_matches(')').trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_where_claude_writes_a_background_commands_output() {
        let text = "Command running in background with ID: b8knmzt35. Output is being written to: /tmp/claude-1000/-tmp-bgtest/6afb/tasks/b8knmzt35.output. You will be notified when it completes.";
        assert_eq!(
            background_result(text),
            Some((
                "b8knmzt35".into(),
                PathBuf::from("/tmp/claude-1000/-tmp-bgtest/6afb/tasks/b8knmzt35.output")
            ))
        );
        assert_eq!(background_result("total 0"), None);
    }

    #[test]
    fn reads_the_exit_code_from_the_summary() {
        assert_eq!(
            exit_code("Background command \"Run three ticks\" completed (exit code 0)"),
            Some(0)
        );
        assert_eq!(exit_code("failed (exit code 101)"), Some(101));
        assert_eq!(exit_code("was stopped"), None);
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use mux_core::Provider;
    use mux_core::protocol::{AgentItem, BackgroundState, ItemStatus};
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::super::{Agent, Process};

    // the events claude sent for a real background command, replayed through the agent
    #[test]
    fn follows_a_claude_background_command_from_start_to_exit() {
        let file = std::env::temp_dir().join(format!("mux-bg-{}.output", std::process::id()));
        std::fs::write(&file, "tick 1\ntick 2\n").unwrap();
        let mut agent = Agent::load(std::path::Path::new("/nonexistent"));
        agent.settings.provider = Provider::Claude;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));

        agent.handle(json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": "toolu_1", "name": "Bash",
            "input": { "command": "pnpm discord", "description": "Start the Discord bot", "run_in_background": true } }] } }));
        agent.handle(json!({ "type": "system", "subtype": "task_started", "task_id": "b8k", "tool_use_id": "toolu_1",
            "description": "Start the Discord bot", "is_backgrounded": true, "task_type": "local_bash" }));
        agent.handle(json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": "toolu_1", "is_error": false,
            "content": format!("Command running in background with ID: b8k. Output is being written to: {}. You will be notified when it completes.", file.display()) }] } }));

        // the row keeps running instead of ending with the "running in background" reply
        assert!(matches!(
            &agent.items[0],
            AgentItem::Command {
                status: ItemStatus::Running,
                background: true,
                ..
            }
        ));
        let tasks = agent.take_background().unwrap();
        assert_eq!(
            (tasks[0].id.as_str(), tasks[0].label.as_str()),
            ("b8k", "Start the Discord bot")
        );
        assert!(tasks[0].stoppable);

        agent.background_output("b8k", "tick 1\ntick 2\n".into());
        assert!(
            matches!(&agent.items[0], AgentItem::Command { output, .. } if output == "tick 1\ntick 2\n")
        );

        agent.stop_background("b8k");
        let sent: Vec<serde_json::Value> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|line| serde_json::from_str(&line).unwrap())
            .collect();
        assert!(sent.iter().any(|m| m["request"]["subtype"] == "stop_task" && m["request"]["task_id"] == "b8k"));

        std::fs::write(&file, "tick 1\ntick 2\ntick 3\n[exited with code 0]\n").unwrap();
        agent.handle(json!({ "type": "system", "subtype": "task_updated", "task_id": "b8k", "patch": { "status": "completed" } }));
        agent.handle(json!({ "type": "system", "subtype": "task_notification", "task_id": "b8k", "tool_use_id": "toolu_1",
            "status": "completed", "output_file": file.display().to_string(),
            "summary": "Background command \"Start the Discord bot\" completed (exit code 0)" }));
        let tasks = agent.background();
        assert_eq!(tasks[0].state, BackgroundState::Exited(Some(0)));
        assert!(!tasks[0].stoppable);
        assert!(
            matches!(&agent.items[0], AgentItem::Command { status: ItemStatus::Done, exit_code: Some(0), output, .. }
            if output.contains("tick 3"))
        );

        // finished ones stay listed until the next message
        agent.clear_finished_background();
        assert!(agent.background().is_empty());
        let _ = std::fs::remove_file(&file);
    }
}
