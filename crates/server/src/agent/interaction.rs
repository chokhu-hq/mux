use mux_core::protocol::{AgentItem, Question, QuestionOption, TodoStatus, TodoStep};
use serde_json::{Value, json};

use super::Agent;

// a question the agent is waiting on, with what's needed to send the answer back
pub(super) struct Asked {
    request: Value,
    index: usize,
    // claude echoes its questions back with the answers, codex wants them keyed by its question ids
    input: Value,
    ids: Vec<String>,
}

// what claude is told when plan mode catches its plan, so it stops rather than carrying on
const PLAN_CAUGHT: &str = "Mux showed your proposed plan to the user. Stop here and wait for their feedback, or for them to ask you to carry it out.";

impl Agent {
    fn ask(&mut self, request: Value, questions: Vec<Question>, input: Value) {
        self.next_approval += 1;
        let id = self.next_approval;
        let ids = questions.iter().map(|q| q.id.clone()).collect();
        let index = self.push(AgentItem::Question {
            id,
            questions,
            answers: None,
        });
        self.questions.insert(
            id,
            Asked {
                request,
                index,
                input,
                ids,
            },
        );
    }

    // codex's request_user_input, where each question has an id the answer is keyed by
    pub(super) fn codex_question(&mut self, request: Value, params: &Value) {
        let questions = params["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|q| Question {
                id: q["id"].as_str().unwrap_or_default().to_owned(),
                header: q["header"].as_str().unwrap_or_default().to_owned(),
                question: q["question"].as_str().unwrap_or_default().to_owned(),
                options: options(&q["options"]),
                multiple: false,
                other: q["isOther"] == true || q["options"].as_array().is_none_or(Vec::is_empty),
            })
            .collect();
        self.ask(request, questions, Value::Null);
    }

    // claude's AskUserQuestion tool, which asks for permission to run and gets its answers in the reply
    pub(super) fn claude_question(&mut self, request: Value, input: Value) {
        let questions = input["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|q| Question {
                // claude keys answers by the question's own text
                id: q["question"].as_str().unwrap_or_default().to_owned(),
                header: q["header"].as_str().unwrap_or_default().to_owned(),
                question: q["question"].as_str().unwrap_or_default().to_owned(),
                options: options(&q["options"]),
                multiple: q["multiSelect"] == true,
                other: true,
            })
            .collect();
        self.ask(request, questions, input);
    }

    pub fn answer(&mut self, question: u64, answers: Vec<Vec<String>>) {
        let Some(asked) = self.questions.remove(&question) else {
            return;
        };
        if self.settings.provider == mux_core::Provider::Claude {
            let keyed: serde_json::Map<String, Value> = asked
                .ids
                .iter()
                .zip(&answers)
                .map(|(id, picks)| (id.clone(), json!(picks.join(", "))))
                .collect();
            self.write(json!({
                "type": "control_response",
                "response": { "subtype": "success", "request_id": asked.request, "response": {
                    "behavior": "allow",
                    "updatedInput": { "questions": asked.input["questions"], "answers": keyed },
                } }
            }));
        } else {
            let keyed: serde_json::Map<String, Value> = asked
                .ids
                .iter()
                .zip(&answers)
                .map(|(id, picks)| (id.clone(), json!({ "answers": picks })))
                .collect();
            self.write(json!({ "id": asked.request, "result": { "answers": keyed } }));
        }
        if let Some(AgentItem::Question { answers: slot, .. }) = self.items.get_mut(asked.index) {
            *slot = Some(answers);
            self.changed.insert(asked.index);
        }
    }

    // questions the agent stopped waiting on, like when its turn ended, are marked as never answered
    pub(super) fn expire_questions(&mut self) {
        for (_, asked) in self.questions.drain() {
            if let Some(AgentItem::Question { answers, .. }) = self.items.get_mut(asked.index) {
                *answers = Some(Vec::new());
                self.changed.insert(asked.index);
            }
        }
    }

    // claude asks to leave plan mode with its plan, which mux keeps and answers by asking it to wait
    pub(super) fn claude_plan(&mut self, request: Value, input: &Value) {
        let text = input["plan"].as_str().unwrap_or_default().to_owned();
        self.push(AgentItem::Plan { text });
        self.write(json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request, "response": {
                "behavior": "deny", "message": PLAN_CAUGHT,
            } }
        }));
    }

    // each turn keeps one checklist, updated in place as the agent ticks things off
    pub(super) fn set_todos(&mut self, steps: Vec<TodoStep>) {
        match self.todos {
            Some(index) if matches!(self.items.get(index), Some(AgentItem::Todos { .. })) => {
                self.items[index] = AgentItem::Todos { steps };
                self.changed.insert(index);
            }
            _ => self.todos = Some(self.push(AgentItem::Todos { steps })),
        }
    }

    pub(super) fn claude_todos(&mut self, input: &Value) {
        let steps = input["todos"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| TodoStep {
                text: t["content"].as_str().unwrap_or_default().to_owned(),
                status: match t["status"].as_str() {
                    Some("completed") => TodoStatus::Done,
                    Some("in_progress") => TodoStatus::Active,
                    _ => TodoStatus::Pending,
                },
            })
            .collect();
        self.set_todos(steps);
    }

    pub(super) fn codex_todos(&mut self, params: &Value) {
        let steps = params["plan"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|t| TodoStep {
                text: t["step"].as_str().unwrap_or_default().to_owned(),
                status: match t["status"].as_str() {
                    Some("completed") => TodoStatus::Done,
                    Some("inProgress") => TodoStatus::Active,
                    _ => TodoStatus::Pending,
                },
            })
            .collect();
        self.set_todos(steps);
    }
}

fn options(value: &Value) -> Vec<QuestionOption> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| QuestionOption {
            label: o["label"].as_str().unwrap_or_default().to_owned(),
            description: o["description"].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}
