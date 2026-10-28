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
                }
            }
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
