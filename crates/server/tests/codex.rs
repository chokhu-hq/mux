// codex sessions end to end, from the socket the app uses through the server to a fake codex app-server
mod common;

use common::Server;
use mux_core::protocol::{
    AgentItem, AgentSettings, ClientMsg, Decision, ItemStatus, QueueAction, ServerMsg,
};

fn running_command(server: &Server) -> bool {
    server.items.iter().any(|i| {
        matches!(
            i,
            AgentItem::Command {
                status: ItemStatus::Running,
                ..
            }
        )
    })
}

fn queued(server: &Server) -> bool {
    server.seen.iter().rev().find_map(|m| match m {
        ServerMsg::AgentQueue { queue, .. } => Some(!queue.is_empty()),
        _ => None,
    }) == Some(true)
}

#[test]
fn a_message_gets_a_reply_and_the_turn_ends() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "hello");
    server.wait_idle();
    assert_eq!(server.assistant_text(), ["ok: hello"]);
    assert!(server.errors().is_empty(), "{:?}", server.errors());
    assert_eq!(server.codex_calls("thread/start").len(), 1);
    let turn = &server.codex_calls("turn/start")[0]["params"];
    assert_eq!(turn["input"][0]["text"], "hello");
    assert_eq!(turn["summary"], "auto", "reasoning summaries are asked for");
}

#[test]
fn send_now_while_a_command_runs_steers_the_running_turn() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "scenario:tool");
    server.wait("the command to start", running_command);
    server.say(id, "also this");
    server.wait("the message to queue", queued);
    server.send(ClientMsg::AgentQueue {
        id,
        index: 0,
        action: QueueAction::SendNow,
    });
    server.wait_idle();
    let steer = server.codex_calls("turn/steer");
    assert_eq!(steer.len(), 1, "it joined the running turn");
    assert_eq!(steer[0]["params"]["expectedTurnId"], "turn-1");
    assert_eq!(steer[0]["params"]["input"][0]["text"], "also this");
    assert_eq!(server.codex_calls("turn/start").len(), 1);
}

#[test]
fn a_message_sent_after_the_last_step_waits_for_the_next_turn() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "scenario:tool");
    server.wait("the command to start", running_command);
    server.say(id, "after");
    server.wait("both turns to finish", |s| {
        !s.busy && s.assistant_text().iter().any(|t| t == "ok: after")
    });
    let turns: Vec<String> = server
        .codex_calls("turn/start")
        .iter()
        .map(|t| t["params"]["input"][0]["text"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(turns, ["scenario:tool", "after"]);
}

#[test]
fn stopping_interrupts_the_turn_without_an_error() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "scenario:slow");
    server.wait("the reply to start", |s| {
        s.assistant_text().iter().any(|t| t.contains("more"))
    });
    server.send(ClientMsg::AgentStop { id });
    server.wait("the turn to stop", |s| !s.busy);
    assert!(server.errors().is_empty(), "{:?}", server.errors());
    let interrupt = server.codex_calls("turn/interrupt");
    assert_eq!(interrupt[0]["params"]["turnId"], "turn-1");
}

#[test]
fn an_approval_goes_to_the_app_and_its_answer_back_to_codex() {
    for (decision, answer) in [(Decision::Allow, "accept"), (Decision::Deny, "decline")] {
        let mut server = Server::start();
        let id = server.codex_agent(AgentSettings::default());
        server.say(id, "scenario:approve");
        let approval = server.wait_for("the approval", |msg| match msg {
            ServerMsg::AgentItem {
                item: AgentItem::Approval { id, detail, .. },
                ..
            } => {
                assert_eq!(detail, "bash -lc \"sleep 1\"");
                Some(*id)
            }
            _ => None,
        });
        server.send(ClientMsg::AgentDecide {
            id,
            approval,
            decision,
        });
        server.wait_idle();
        let reply = server
            .codex_stdin()
            .into_iter()
            .find(|m| m["id"] == 900 && m.get("method").is_none())
            .expect("mux answered the approval");
        assert_eq!(reply["result"]["decision"], answer);
    }
}

#[test]
fn a_failed_turn_says_why() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "scenario:fail");
    server.wait("the error", |s| !s.busy && !s.errors().is_empty());
    assert!(
        server.errors()[0].contains("usage limit"),
        "{:?}",
        server.errors()
    );
}

#[test]
fn a_crash_mid_turn_says_so() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "scenario:crash");
    server.wait("the error", |s| !s.errors().is_empty());
    assert!(
        server.errors()[0].contains("stopped unexpectedly"),
        "{:?}",
        server.errors()
    );
}

#[test]
fn a_long_reply_streams_once_not_over_and_over() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "hello");
    server.wait_idle();
    let before = server.received.load(std::sync::atomic::Ordering::Relaxed);
    server.say(id, "scenario:flood");
    server.wait("the flood to finish", |s| {
        !s.busy && s.assistant_text().len() > 2
    });
    let sent = server.received.load(std::sync::atomic::Ordering::Relaxed) - before;
    // 300 KB streamed, then the finished message once more
    assert!(sent < 1024 * 1024, "a 300 KB reply cost {} KB", sent / 1024);
    assert_eq!(server.assistant_text()[1].len(), 300 * 1024);
}

fn continuations(server: &Server) -> usize {
    server
        .codex_calls("turn/start")
        .iter()
        .filter(|t| {
            t["params"]["input"][0]["text"]
                .as_str()
                .is_some_and(|t| t.contains("was cut off"))
        })
        .count()
}

#[test]
fn a_turn_a_crash_cut_off_carries_on_in_the_same_thread() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "scenario:slow");
    server.wait("the reply to start", |s| {
        s.assistant_text().iter().any(|t| t.contains("more"))
    });
    server.crash_and_restart();
    server.wait("the turn to carry on and finish", |s| {
        !s.busy && s.assistant_text().iter().any(|t| t.contains("was cut off"))
    });
    assert_eq!(continuations(&server), 1);
    let resume = server.codex_calls("thread/resume");
    assert_eq!(resume.len(), 1, "it picked the old thread back up");
    let started = server
        .codex_log()
        .into_iter()
        .find_map(|e| {
            let msg: serde_json::Value = serde_json::from_str(e["stdin"].as_str()?).ok()?;
            (msg["method"] == "turn/start").then(|| msg["params"]["threadId"].clone())
        })
        .unwrap();
    assert_eq!(resume[0]["params"]["threadId"], started);
}

#[test]
fn a_thread_codex_lost_carries_on_in_a_new_one() {
    let mut server = Server::start_with(&[("FAKE_CODEX_FORGET", "1")]);
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "scenario:slow");
    server.wait("the reply to start", |s| {
        s.assistant_text().iter().any(|t| t.contains("more"))
    });
    server.crash_and_restart();
    server.wait("the turn to carry on and finish", |s| {
        !s.busy && s.assistant_text().iter().any(|t| t.contains("was cut off"))
    });
    assert_eq!(server.codex_calls("thread/resume").len(), 1);
    assert_eq!(
        server.codex_calls("thread/start").len(),
        2,
        "a fresh thread after the lost one"
    );
    assert!(server.errors().is_empty(), "{:?}", server.errors());
}

#[test]
fn a_turn_that_keeps_crashing_the_server_stops_after_three_tries() {
    let mut server = Server::start_with(&[("FAKE_CODEX_STALL", "1")]);
    let id = server.codex_agent(AgentSettings::default());
    let streaming = |s: &Server| s.assistant_text().iter().any(|t| t.contains("more"));
    server.say(id, "scenario:slow");
    server.wait("the reply to start", streaming);
    for _ in 0..3 {
        server.crash_and_restart();
        server.wait("the reply to start", streaming);
    }
    server.crash_and_restart();
    server.wait("mux to give up", |s| !s.errors().is_empty());
    assert!(
        server.errors()[0].contains("3 times in a row"),
        "{:?}",
        server.errors()
    );
    assert_eq!(continuations(&server), 3);
}

#[test]
fn a_turn_a_usage_limit_stopped_carries_on_after_a_restart_too() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.say(id, "scenario:limit");
    server.wait("the limit", |s| {
        s.errors().iter().any(|e| e.contains("carry on by itself"))
    });
    // the wake-up is saved, so a server that restarts meanwhile still carries on
    server.crash_and_restart();
    server.wait("the turn to carry on", |s| {
        !s.busy
            && s.assistant_text()
                .iter()
                .any(|t| t.contains("limit has reset"))
    });
    assert_eq!(server.codex_calls("thread/resume").len(), 1);
}
