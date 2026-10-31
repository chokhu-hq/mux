use mux_core::Provider;
use mux_core::protocol::AgentItem;
use serde_json::json;

use super::{Agent, Call};

impl Agent {
    // takes the conversation back to before the user's `message`th message, for it to be edited and sent again,
    // returning false when the agent is busy or there's no such message
    pub fn rewind(&mut self, message: usize) -> bool {
        if self.busy {
            return false;
        }
        let Some((index, point)) = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| match item {
                AgentItem::User { rewind, .. } => Some((i, rewind.clone())),
                _ => None,
            })
            .nth(message)
        else {
            return false;
        };
        self.items.truncate(index);
        // everything that pointed into the dropped part of the transcript goes with it
        self.changed.clear();
        self.by_codex_id.retain(|_, i| *i < index);
        self.sub_steps.retain(|_, (i, _)| *i < index);
        self.tasks.retain(|_, i| *i < index);
        self.child_threads.retain(|_, i| *i < index);
        self.live_text = None;
        self.compacting = None;
        self.todos = None;
        if self.settings.provider == Provider::Claude {
            // claude starts again at the reply before the message, or afresh when it was the first
            match point.filter(|head| !head.is_empty()) {
                Some(head) => {
                    self.claude_head = Some(head.clone());
                    self.resume_at = Some(head);
                }
                None => {
                    self.claude_head = None;
                    self.session_id = None;
                    self.resume_at = None;
                }
            }
            self.process = None;
        } else if self.settings.provider == Provider::OpenCode {
            self.session_id = None;
            self.thread_live = false;
            self.process = None;
        } else {
            match point {
                Some(turn) => {
                    if self.thread_live {
                        self.revert(turn);
                    } else {
                        self.pending_revert = Some(turn);
                    }
                }
                // without the turn on record codex can't be wound back, so it starts a new thread instead
                None => {
                    self.thread_id = None;
                    self.pending_revert = None;
                    self.thread_live = false;
                    self.process = None;
                }
            }
        }
        true
    }

    pub(super) fn revert(&mut self, turn: String) {
        if let Some(thread) = self.thread_id.clone() {
            self.call(
                Call::Revert,
                "thread/revert",
                json!({ "threadId": thread, "beforeTurnId": turn }),
            );
        }
    }

    // codex names a turn only once it starts, so the messages that started it are filled in then
    pub(super) fn mark_turn(&mut self, turn: &str) {
        for item in self.items.iter_mut().rev() {
            if let AgentItem::User { rewind, .. } = item {
                if rewind.is_some() {
                    break;
                }
                *rewind = Some(turn.to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Process, claude_args};
    use super::*;
    use tokio::sync::mpsc;

    fn conversation(provider: Provider) -> Agent {
        let mut agent = Agent::load(std::path::Path::new("/nonexistent"));
        agent.settings.provider = provider;
        agent.session_id = Some("session".into());
        agent.thread_id = Some("thread".into());
        agent.items = vec![
            AgentItem::User {
                text: "first".into(),
                attachments: Vec::new(),
                queued: false,
                rewind: None,
            },
            AgentItem::Assistant {
                text: "reply".into(),
            },
            AgentItem::User {
                text: "second".into(),
                attachments: Vec::new(),
                queued: false,
                rewind: match provider {
                    Provider::Claude => Some("reply".into()),
                    Provider::Codex => Some("turn".into()),
                    Provider::OpenCode => None,
                },
            },
        ];
        agent
    }

    #[test]
    fn opencode_rewind_starts_a_new_session() {
        let mut agent = conversation(Provider::OpenCode);
        agent.thread_live = true;
        let (tx, _) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        assert!(agent.rewind(1));
        assert_eq!(agent.items.len(), 2);
        assert!(agent.process.is_none());
        assert!(!agent.thread_live);
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.opencode_initialize();
        agent.handle(json!({ "id": 0, "result": {} }));
        let sent: Vec<serde_json::Value> = std::iter::from_fn(|| rx.try_recv().ok())
            .map(|line| serde_json::from_str(&line).unwrap())
            .collect();
        assert_eq!(sent[1]["method"], "session/new");
        assert!(sent.iter().all(|msg| msg["method"] != "session/resume"));
    }

    #[test]
    fn pending_rewinds_survive_a_restart() {
        for provider in [Provider::Claude, Provider::Codex] {
            let mut agent = conversation(provider);
            assert!(agent.rewind(1));
            let dir = std::env::temp_dir().join(format!(
                "mux-rewind-{}-{}",
                std::process::id(),
                provider.label()
            ));
            let path = dir.join("agent.json");
            agent.save(&path).unwrap();
            let mut loaded = Agent::load(&path);
            assert_eq!(loaded.items, agent.items);
            match provider {
                Provider::Claude => {
                    let args = claude_args(
                        &loaded.settings,
                        loaded.session_id.as_deref(),
                        loaded.resume_at.as_deref(),
                    );
                    assert!(
                        args.windows(2)
                            .any(|args| args == ["--resume-session-at", "reply"])
                    );
                }
                Provider::Codex => {
                    let (tx, mut rx) = mpsc::unbounded_channel();
                    loaded.process = Some(Process::fake(tx));
                    loaded.calls.insert(0, Call::ResumeThread);
                    loaded.handle(json!({ "id": 0, "result": { "thread": { "id": "thread" } } }));
                    let sent: serde_json::Value =
                        serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
                    assert_eq!(sent["method"], "thread/revert");
                    assert_eq!(sent["params"]["beforeTurnId"], "turn");
                }
                Provider::OpenCode => unreachable!(),
            }
            assert!(loaded.rewind(0));
            assert!(loaded.resume_at.is_none());
            assert!(loaded.pending_revert.is_none());
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
