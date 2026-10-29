// a stand-in for `codex app-server` that the server tests put first on PATH, so they run mux's real code for free.
// it answers the same json-rpc as codex, picks what to do from the words in each message, and writes everything
// it was started with and sent to the file in FAKE_CODEX_LOG, so a test can check what mux did.
//
// scenarios, chosen by a word anywhere in the message:
//   scenario:tool     runs a command for a moment, taking messages steered in meanwhile
//   scenario:approve  asks permission to run a command and waits for the answer
//   scenario:slow     streams a long reply until it's interrupted
//   scenario:fail     ends the turn as failed with a message
//   scenario:limit    hits a usage limit that resets a second later
//   scenario:retry    reconnects a few times before answering, or giving up with retry-fail
//   scenario:crash    dies mid-turn
//   scenario:flood    streams a 300 KB reply as fast as it can
// anything else gets a short reply. FAKE_CODEX_FORGET makes thread/resume fail, like a thread codex lost,
// and FAKE_CODEX_STALL makes a turn mux carries on after a crash stall too
use std::io::{BufRead, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    log(json!({ "argv": args }));
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
        thread: None,
        turns: 0,
        turn: None,
        interrupted: false,
        steered: Vec::new(),
        answers: Vec::new(),
        next_request: 900,
    };
    fake.run();
}

struct Fake {
    inbox: Receiver<String>,
    thread: Option<String>,
    turns: u32,
    // the turn running now
    turn: Option<String>,
    interrupted: bool,
    // input steered into the running turn, read at its next step
    steered: Vec<String>,
    // replies to requests this fake sent, by id
    answers: Vec<(u64, Value)>,
    next_request: u64,
}

impl Fake {
    fn run(&mut self) {
        loop {
            let Some(turn) = self.next(None) else { return };
            if let Some((id, text)) = turn {
                self.turn(id, text);
            }
        }
    }

    // takes one message off stdin, answering calls on the way. a turn/start comes back to be run, and with a
    // deadline it returns Some(None) once the time is up. none means stdin closed
    fn next(&mut self, until: Option<Instant>) -> Option<Option<(Value, String)>> {
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
            let id = msg["id"].clone();
            let Some(method) = msg["method"].as_str() else {
                // a reply to a request this fake sent, like an approval
                if let Some(id) = id.as_u64() {
                    self.answers.push((id, msg["result"].clone()));
                }
                continue;
            };
            match method {
                "initialize" => reply(&id, json!({ "userAgent": "fake-codex" })),
                "thread/start" => {
                    let thread = format!("th-{}", std::process::id());
                    self.thread = Some(thread.clone());
                    reply(&id, json!({ "thread": { "id": thread } }));
                }
                "thread/resume" => {
                    if std::env::var_os("FAKE_CODEX_FORGET").is_some() {
                        fail(&id, "no rollout found for thread");
                    } else {
                        let thread = msg["params"]["threadId"].as_str().unwrap_or_default();
                        self.thread = Some(thread.to_owned());
                        reply(&id, json!({ "thread": { "id": thread } }));
                    }
                }
                "turn/start" => {
                    if until.is_some() {
                        fail(&id, "a turn is already running");
                        continue;
                    }
                    return Some(Some((id, input_text(&msg["params"]["input"]))));
                }
                "turn/steer" => {
                    let expected = msg["params"]["expectedTurnId"].as_str();
                    if self.turn.is_some() && expected == self.turn.as_deref() {
                        self.steered.push(input_text(&msg["params"]["input"]));
                        reply(&id, json!({ "turnId": self.turn }));
                    } else {
                        fail(&id, "no active turn to steer");
                    }
                }
                "turn/interrupt" => {
                    self.interrupted = self.turn.is_some();
                    reply(&id, json!({}));
                }
                "model/list" => reply(
                    &id,
                    json!({ "data": [{ "id": "fake-codex-1", "displayName": "Fake Codex", "description": "For tests",
                        "isDefault": true, "supportedReasoningEfforts": [], "defaultReasoningEffort": "low" }] }),
                ),
                "account/rateLimits/read" => reply(&id, json!({ "rateLimits": {} })),
                "skills/list" => reply(&id, json!({ "data": [] })),
                _ if !id.is_null() => reply(&id, json!({})),
                _ => {}
            }
        }
    }

    // waits a while like a command or a stream would, and says whether the turn was interrupted meanwhile
    fn pause(&mut self, ms: u64) -> bool {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until && !self.interrupted {
            if self.next(Some(until)).is_none() {
                std::process::exit(0);
            }
        }
        self.interrupted
    }

    fn turn(&mut self, id: Value, text: String) {
        self.turns += 1;
        let turn = format!("turn-{}", self.turns);
        self.turn = Some(turn.clone());
        self.interrupted = false;
        reply(&id, json!({ "turn": { "id": turn } }));
        note(
            "turn/started",
            json!({ "threadId": self.thread, "turn": { "id": turn } }),
        );
        note(
            "item/started",
            json!({ "item": { "type": "userMessage", "id": format!("{turn}-user") } }),
        );
        if text.contains("scenario:crash") {
            eprintln!("fake codex crashed on purpose");
            std::process::exit(3);
        }
        if text.contains("scenario:retry") {
            for attempt in 1..=2 {
                note(
                    "error",
                    json!({ "threadId": self.thread, "turnId": turn, "willRetry": true,
                    "error": { "message": format!("Reconnecting... {attempt}/5 (stream disconnected)") } }),
                );
                if self.pause(60) {
                    return self.interrupted_end(&turn);
                }
            }
            if text.contains("scenario:retry-fail") {
                return self.end(json!({ "id": turn, "status": "failed",
                    "error": { "message": "stream disconnected before completion" } }));
            }
        }
        if text.contains("scenario:limit") {
            let resets = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs())
                + 1;
            note(
                "account/rateLimits/updated",
                json!({ "rateLimits": {
                "primary": { "usedPercent": 100, "windowDurationMins": 300, "resetsAt": resets },
                "secondary": { "usedPercent": 40, "windowDurationMins": 10080, "resetsAt": resets + 999 }
            } }),
            );
            return self.end(json!({ "id": turn, "status": "failed", "error": {
                "message": "You've hit your usage limit.", "codexErrorInfo": "usageLimitExceeded" } }));
        }
        if text.contains("scenario:fail") {
            return self.end(json!({ "id": turn, "status": "failed",
                "error": { "message": "You've hit your usage limit. Try again at 3:40 PM." } }));
        }
        if text.contains("scenario:tool") || text.contains("scenario:approve") {
            let command = format!("{turn}-cmd");
            note(
                "item/started",
                json!({ "item": { "type": "commandExecution", "id": command,
                "command": "bash -lc \"sleep 1\"", "status": "inProgress" } }),
            );
            let mut status = "completed";
            if text.contains("scenario:approve") {
                let request = self.next_request;
                self.next_request += 1;
                say(
                    json!({ "id": request, "method": "item/commandExecution/requestApproval",
                    "params": { "threadId": self.thread, "turnId": turn, "itemId": command } }),
                );
                let answer = self.wait_for_answer(request);
                if answer["decision"] != "accept" && answer["decision"] != "acceptForSession" {
                    status = "declined";
                }
            }
            if status == "completed" && self.pause(400) {
                return self.interrupted_end(&turn);
            }
            note(
                "item/completed",
                json!({ "item": { "type": "commandExecution", "id": command,
                "status": status, "exitCode": if status == "completed" { 0 } else { 1 }, "aggregatedOutput": "" } }),
            );
            for steered in std::mem::take(&mut self.steered) {
                note(
                    "item/started",
                    json!({ "item": { "type": "userMessage", "id": format!("{turn}-steer"), "text": steered } }),
                );
            }
        }
        let resumed =
            text.contains("was cut off") && std::env::var_os("FAKE_CODEX_STALL").is_some();
        if text.contains("scenario:slow") || resumed {
            let message = format!("{turn}-slow");
            note(
                "item/started",
                json!({ "item": { "type": "agentMessage", "id": message, "text": "" } }),
            );
            for _ in 0..200 {
                note(
                    "item/agentMessage/delta",
                    json!({ "itemId": message, "delta": "more " }),
                );
                if self.pause(50) {
                    return self.interrupted_end(&turn);
                }
            }
        }
        if text.contains("scenario:flood") {
            let message = format!("{turn}-flood");
            note(
                "item/started",
                json!({ "item": { "type": "agentMessage", "id": message, "text": "" } }),
            );
            let chunk = "x".repeat(1023) + "\n";
            for _ in 0..300 {
                note(
                    "item/agentMessage/delta",
                    json!({ "itemId": message, "delta": chunk }),
                );
            }
            note(
                "item/completed",
                json!({ "item": { "type": "agentMessage", "id": message,
                "text": chunk.repeat(300) } }),
            );
        }
        let message = format!("{turn}-reply");
        let reply_text = format!("ok: {}", text.trim());
        note(
            "item/started",
            json!({ "item": { "type": "agentMessage", "id": message, "text": "" } }),
        );
        note(
            "item/agentMessage/delta",
            json!({ "itemId": message, "delta": reply_text }),
        );
        note(
            "item/completed",
            json!({ "item": { "type": "agentMessage", "id": message, "text": reply_text } }),
        );
        note(
            "thread/tokenUsage/updated",
            json!({ "tokenUsage": {
            "last": { "totalTokens": 120 }, "modelContextWindow": 272000 } }),
        );
        self.end(json!({ "id": turn, "status": "completed" }));
    }

    fn wait_for_answer(&mut self, request: u64) -> Value {
        loop {
            if let Some(at) = self.answers.iter().position(|(id, _)| *id == request) {
                return self.answers.remove(at).1;
            }
            let until = Instant::now() + Duration::from_millis(100);
            if self.next(Some(until)).is_none() {
                std::process::exit(0);
            }
        }
    }

    fn interrupted_end(&mut self, turn: &str) {
        self.end(json!({ "id": turn, "status": "interrupted" }));
    }

    fn end(&mut self, turn: Value) {
        self.turn = None;
        self.interrupted = false;
        note(
            "turn/completed",
            json!({ "threadId": self.thread, "turn": turn }),
        );
    }
}

fn input_text(input: &Value) -> String {
    input
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn reply(id: &Value, result: Value) {
    say(json!({ "id": id, "result": result }));
}

fn fail(id: &Value, message: &str) {
    say(json!({ "id": id, "error": { "code": -32600, "message": message } }));
}

fn note(method: &str, params: Value) {
    say(json!({ "method": method, "params": params }));
}

fn say(msg: Value) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(format!("{msg}\n").as_bytes());
    let _ = out.flush();
}

// each line says which process wrote it, since mux starts short-lived codexes to list models and skills too
fn log(mut entry: Value) {
    let Some(path) = std::env::var_os("FAKE_CODEX_LOG") else {
        return;
    };
    entry["pid"] = json!(std::process::id());
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(format!("{entry}\n").as_bytes());
    }
}
