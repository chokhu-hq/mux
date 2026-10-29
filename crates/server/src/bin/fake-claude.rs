// a stand-in for the claude cli that the server tests put first on PATH, so they run mux's real code for free.
// it speaks the same stream-json as claude, picks what to do from the words in each message, and writes
// everything it was started with and sent to the file in FAKE_CLAUDE_LOG, so a test can check what mux did.
//
// scenarios, chosen by a word anywhere in the message:
//   scenario:tool     runs a command for a moment, picking up messages sent meanwhile after its result
//   scenario:slow     streams a long reply until it's interrupted
//   scenario:approve  asks permission to run a command and waits for the answer
//   scenario:crash    dies mid-turn
//   scenario:auth     fails the way claude does when it's signed out
//   scenario:flood    streams a 300 KB reply as fast as it can
// anything else gets a short reply. FAKE_CLAUDE_FAIL lists control request subtypes to refuse, like set_model.
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let inherited: Vec<String> = std::env::vars()
        .map(|(key, _)| key)
        .filter(|key| key == "CLAUDECODE" || key.starts_with("CLAUDE_CODE_"))
        .collect();
    log(json!({ "argv": args, "env": inherited }));
    let session = args
        .iter()
        .position(|a| a == "--resume")
        .and_then(|i| args.get(i + 1).cloned())
        .unwrap_or_else(|| format!("fake-{}", std::process::id()));
    let (lines, inbox) = mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    let mut fake = Fake {
        inbox,
        session,
        started: false,
        waiting: Vec::new(),
        interrupted: false,
    };
    fake.run();
}

struct Fake {
    inbox: Receiver<String>,
    session: String,
    started: bool,
    // user messages that arrived mid-turn, which claude reads after the next tool result or once the turn ends
    waiting: Vec<Value>,
    interrupted: bool,
}

impl Fake {
    fn run(&mut self) {
        loop {
            let message = match self.waiting.is_empty() {
                true => match self.next(None) {
                    Some(Some(message)) => message,
                    Some(None) => continue,
                    None => return,
                },
                false => self.waiting.remove(0),
            };
            self.turn(message);
        }
    }

    // the next user message from stdin, handling control requests on the way, or none once stdin closes.
    // with a deadline it returns Some(None) when the time is up, and user messages are kept for later
    fn next(&mut self, until: Option<Instant>) -> Option<Option<Value>> {
        loop {
            let line = match until {
                None => self.inbox.recv().ok()?,
                Some(until) => {
                    let left = until.saturating_duration_since(Instant::now());
                    match self.inbox.recv_timeout(left) {
                        Ok(line) => line,
                        Err(RecvTimeoutError::Timeout) => return Some(None),
                        Err(RecvTimeoutError::Disconnected) => return None,
                    }
                }
            };
            log(json!({ "stdin": line }));
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match msg["type"].as_str() {
                Some("control_request") => self.control(&msg),
                Some("user") if until.is_some() => self.waiting.push(msg),
                Some("user") => return Some(Some(msg)),
                _ => {}
            }
        }
    }

    // waits a while like a tool or a stream would, and says whether the turn was interrupted meanwhile
    fn pause(&mut self, ms: u64) -> bool {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            if self.next(Some(until)).is_none() {
                std::process::exit(0);
            }
            if self.interrupted || self.urgent() {
                return true;
            }
        }
        self.interrupted
    }

    // a message sent with priority "now" breaks into whatever claude is writing
    fn urgent(&self) -> bool {
        self.waiting.iter().any(|m| m["priority"] == "now")
    }

    fn control(&mut self, msg: &Value) {
        let id = msg["request_id"].clone();
        let subtype = msg["request"]["subtype"].as_str().unwrap_or_default();
        let refused = std::env::var("FAKE_CLAUDE_FAIL")
            .is_ok_and(|fail| fail.split(',').any(|s| s.trim() == subtype));
        if refused {
            say(json!({ "type": "control_response", "response": {
                "subtype": "error", "request_id": id, "error": format!("fake claude refuses {subtype}")
            } }));
            return;
        }
        let response = match subtype {
            "initialize" => json!({
                "commands": [{ "name": "review", "description": "Review a pull request" }],
                "models": [
                    { "value": "default", "displayName": "Default (recommended)",
                      "description": "Fake 1 · For tests", "supportedEffortLevels": ["low", "high"] },
                    { "value": "fake-2", "displayName": "Fake 2", "description": "The other one" }
                ],
            }),
            "interrupt" => {
                self.interrupted = true;
                json!({})
            }
            _ => json!({}),
        };
        say(json!({ "type": "control_response", "response": {
            "subtype": "success", "request_id": id, "response": response
        } }));
    }

    fn turn(&mut self, message: Value) {
        self.interrupted = false;
        if !self.started {
            self.started = true;
            say(
                json!({ "type": "system", "subtype": "init", "session_id": self.session, "skills": [] }),
            );
        }
        let mut message = message;
        message["isReplay"] = json!(true);
        say(message.clone());
        let text = text_of(&message);
        if text.contains("scenario:crash") {
            eprintln!("fake claude crashed on purpose");
            std::process::exit(3);
        }
        if text.contains("scenario:auth") {
            say(
                json!({ "type": "assistant", "error": "authentication_failed", "message": {
                "content": [{ "type": "text", "text": "Not logged in · Please run /login" }]
            } }),
            );
            say(
                json!({ "type": "result", "subtype": "success", "is_error": true,
                "result": "Not logged in · Please run /login", "session_id": self.session }),
            );
            return;
        }
        if text.contains("scenario:tool") || text.contains("scenario:approve") {
            let command = "sleep 1";
            say(
                json!({ "type": "assistant", "uuid": format!("{}-tool", self.session), "message": {
                "content": [{ "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": command } }]
            } }),
            );
            let mut output = json!("done");
            let mut declined = false;
            if text.contains("scenario:approve") {
                say(
                    json!({ "type": "control_request", "request_id": "perm-1", "request": {
                    "subtype": "can_use_tool", "tool_name": "Bash", "input": { "command": command },
                    "permission_suggestions": []
                } }),
                );
                let answer = self.wait_for_answer("perm-1");
                declined = answer["behavior"] != "allow";
                if declined {
                    output = json!("The user declined this.");
                }
            }
            if !declined && self.pause(400) {
                return self.aborted("aborted_tools");
            }
            say(json!({ "type": "user", "message": { "content": [
                { "type": "tool_result", "tool_use_id": "t1", "content": output, "is_error": declined }
            ] } }));
            // whatever arrived while the tool ran is read now, as part of this turn
            for waiting in std::mem::take(&mut self.waiting) {
                let mut waiting = waiting;
                waiting["isReplay"] = json!(true);
                say(waiting);
            }
        }
        if text.contains("scenario:slow") {
            self.stream_start();
            for _ in 0..200 {
                self.stream_delta("more ");
                if self.pause(50) && !self.urgent() {
                    return self.aborted("aborted_streaming");
                }
                if self.urgent() {
                    break;
                }
            }
            for waiting in std::mem::take(&mut self.waiting) {
                let mut waiting = waiting;
                waiting["isReplay"] = json!(true);
                say(waiting);
            }
        }
        if text.contains("scenario:flood") {
            self.stream_start();
            let chunk = "x".repeat(1023) + "\n";
            for _ in 0..300 {
                self.stream_delta(&chunk);
            }
        }
        let reply = format!("ok: {}", text.trim());
        self.stream_start();
        self.stream_delta(&reply);
        say(
            json!({ "type": "assistant", "uuid": format!("{}-reply", self.session), "message": {
            "content": [{ "type": "text", "text": reply }],
            "usage": { "input_tokens": 10, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "output_tokens": 5 }
        } }),
        );
        say(
            json!({ "type": "result", "subtype": "success", "is_error": false, "result": reply,
            "session_id": self.session,
            "modelUsage": { "fake-1": { "contextWindow": 200000 } } }),
        );
    }

    fn wait_for_answer(&mut self, request: &str) -> Value {
        loop {
            let Ok(line) = self.inbox.recv() else {
                std::process::exit(0);
            };
            log(json!({ "stdin": line }));
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match msg["type"].as_str() {
                Some("control_response") if msg["response"]["request_id"] == request => {
                    return msg["response"]["response"].clone();
                }
                Some("control_request") => self.control(&msg),
                Some("user") => self.waiting.push(msg),
                _ => {}
            }
        }
    }

    fn stream_start(&self) {
        say(json!({ "type": "stream_event", "event": {
            "type": "content_block_start", "content_block": { "type": "text" }
        } }));
    }

    fn stream_delta(&self, text: &str) {
        say(json!({ "type": "stream_event", "event": {
            "type": "content_block_delta", "delta": { "type": "text_delta", "text": text }
        } }));
    }

    fn aborted(&mut self, reason: &str) {
        self.interrupted = false;
        say(
            json!({ "type": "result", "subtype": "error_during_execution", "is_error": true,
            "result": null, "terminal_reason": reason, "session_id": self.session }),
        );
    }
}

fn text_of(message: &Value) -> String {
    match &message["message"]["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn say(msg: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

// each line says which process wrote it, since mux starts a short-lived claude to list models too
fn log(mut entry: Value) {
    let Some(path) = std::env::var_os("FAKE_CLAUDE_LOG") else {
        return;
    };
    entry["pid"] = json!(std::process::id());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        // one write per line, since the model listing and the agent can both be logging at once
        let _ = file.write_all(format!("{entry}\n").as_bytes());
    }
}
