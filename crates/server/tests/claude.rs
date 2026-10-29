// claude sessions end to end, from the socket the app uses through the server to a fake claude cli
mod common;

use common::Server;
use mux_core::protocol::{AgentItem, AgentSettings, ClientMsg, Decision, ItemStatus};

#[test]
fn a_message_gets_a_reply_and_the_turn_ends() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "hello");
    server.wait_idle();
    assert_eq!(server.assistant_text(), ["ok: hello"]);
    assert!(server.errors().is_empty(), "{:?}", server.errors());
    let launches = server.claude_launches();
    assert_eq!(launches.len(), 1, "one process for the conversation");
}

#[test]
fn a_message_sent_while_a_tool_runs_goes_once_claude_is_free() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:tool");
    server.wait("the command to start", |s| {
        s.items.iter().any(|i| {
            matches!(
                i,
                AgentItem::Command {
                    status: ItemStatus::Running,
                    ..
                }
            )
        })
    });
    server.say(id, "also this");
    server.wait_idle();
    let sent: Vec<String> = server
        .claude_stdin()
        .iter()
        .filter(|m| m["type"] == "user")
        .map(|m| {
            m["message"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(sent, ["scenario:tool", "also this"]);
    assert_eq!(server.claude_launches().len(), 1);
}

#[test]
fn stopping_a_turn_is_not_an_error() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:slow");
    server.wait("the reply to start", |s| {
        s.assistant_text().iter().any(|t| t.contains("more"))
    });
    server.send(ClientMsg::AgentStop { id });
    server.wait("the turn to stop", |s| !s.busy);
    assert!(server.errors().is_empty(), "{:?}", server.errors());
    assert!(
        server
            .claude_stdin()
            .iter()
            .any(|m| m["request"]["subtype"] == "interrupt")
    );
}

#[test]
fn a_crash_mid_turn_says_so() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:crash");
    server.wait("the error", |s| !s.errors().is_empty());
    assert!(server.errors()[0].contains("stopped unexpectedly"));
}

#[test]
fn being_signed_out_shows_what_to_do() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:auth");
    server.wait("the error", |s| !s.errors().is_empty());
    assert!(
        server.errors()[0].contains("/login"),
        "{:?}",
        server.errors()
    );
}

#[test]
fn an_approval_goes_to_the_app_and_its_answer_back_to_claude() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:approve");
    let approval = server.wait_for("the approval", |msg| match msg {
        mux_core::protocol::ServerMsg::AgentItem {
            item: AgentItem::Approval { id, .. },
            ..
        } => Some(*id),
        _ => None,
    });
    server.send(ClientMsg::AgentDecide {
        id,
        approval,
        decision: Decision::Deny,
    });
    server.wait_idle();
    let answer = server
        .claude_stdin()
        .into_iter()
        .find(|m| m["type"] == "control_response")
        .expect("mux answered the permission request");
    assert_eq!(answer["response"]["request_id"], "perm-1");
    assert_eq!(answer["response"]["response"]["behavior"], "deny");
}

#[test]
fn claude_is_told_to_spare_background_agents_when_a_turn_stops() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "hello");
    server.wait_idle();
    let first = server.claude_stdin().into_iter().next().unwrap();
    assert_eq!(first["request"]["subtype"], "initialize");
    assert_eq!(first["request"]["perTaskStopAffordance"], true);
}

fn priorities(server: &Server) -> Vec<(String, serde_json::Value)> {
    server
        .claude_stdin()
        .iter()
        .filter(|m| m["type"] == "user")
        .map(|m| {
            let text = m["message"]["content"][0]["text"]
                .as_str()
                .unwrap_or_default();
            (text.to_owned(), m["priority"].clone())
        })
        .collect()
}

#[test]
fn send_now_breaks_into_a_reply_but_waits_for_a_running_tool() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:slow");
    server.wait("the reply to start", |s| {
        s.assistant_text().iter().any(|t| t.contains("more"))
    });
    server.say(id, "change of plan");
    server.wait("the message to queue", |s| {
        s.seen.iter().any(|m| matches!(m, mux_core::protocol::ServerMsg::AgentQueue { queue, .. } if !queue.is_empty()))
    });
    server.send(ClientMsg::AgentQueue {
        id,
        index: 0,
        action: mux_core::protocol::QueueAction::SendNow,
    });
    server.wait_idle();
    assert_eq!(
        priorities(&server),
        [
            ("scenario:slow".to_owned(), serde_json::Value::Null),
            ("change of plan".to_owned(), "now".into()),
        ]
    );
    assert!(server.errors().is_empty(), "{:?}", server.errors());

    server.say(id, "scenario:tool");
    server.wait("the command to start", |s| {
        s.items.iter().any(|i| {
            matches!(
                i,
                AgentItem::Command {
                    status: ItemStatus::Running,
                    ..
                }
            )
        })
    });
    server.say(id, "after the tool");
    server.wait("the message to queue", |s| {
        s.seen.iter().rev().find_map(|m| match m {
            mux_core::protocol::ServerMsg::AgentQueue { queue, .. } => Some(!queue.is_empty()),
            _ => None,
        }) == Some(true)
    });
    server.send(ClientMsg::AgentQueue {
        id,
        index: 0,
        action: mux_core::protocol::QueueAction::SendNow,
    });
    server.wait("both to finish", |s| {
        !s.busy && s.assistant_text().len() >= 3
    });
    assert_eq!(
        priorities(&server)[3],
        ("after the tool".to_owned(), "next".into())
    );
}

fn configure(server: &mut Server, id: u64, settings: AgentSettings) {
    server.send(ClientMsg::AgentConfigure {
        id,
        settings: AgentSettings {
            provider: mux_core::Provider::Claude,
            ..settings
        },
    });
}

fn reply(server: &mut Server, id: u64, text: &str) {
    let replies = server.assistant_text().len();
    server.say(id, text);
    server.wait("the reply", |s| {
        !s.busy && s.assistant_text().len() > replies
    });
}

#[test]
fn new_settings_reach_the_running_claude_without_a_relaunch() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    reply(&mut server, id, "first");
    configure(
        &mut server,
        id,
        AgentSettings {
            model: Some("fake-2".into()),
            effort: Some("high".into()),
            plan: true,
            ..AgentSettings::default()
        },
    );
    reply(&mut server, id, "second");
    assert_eq!(server.claude_launches().len(), 1, "still the same process");
    let sent = server.claude_stdin();
    let subtypes: Vec<&str> = sent
        .iter()
        .filter_map(|m| m["request"]["subtype"].as_str())
        .collect();
    assert_eq!(
        subtypes,
        [
            "initialize",
            "set_model",
            "apply_flag_settings",
            "set_permission_mode"
        ]
    );
    let request = |subtype: &str| {
        sent.iter()
            .find(|m| m["request"]["subtype"] == subtype)
            .unwrap()["request"]
            .clone()
    };
    assert_eq!(request("set_model")["model"], "fake-2");
    assert_eq!(
        request("apply_flag_settings")["settings"]["effortLevel"],
        "high"
    );
    assert_eq!(request("set_permission_mode")["mode"], "plan");
    let second = sent
        .iter()
        .position(|m| m["message"]["content"][0]["text"] == "second")
        .unwrap();
    let last_change = sent
        .iter()
        .rposition(|m| m["type"] == "control_request")
        .unwrap();
    assert!(
        last_change < second,
        "the settings go ahead of the message they're for"
    );
}

#[test]
fn a_stricter_access_level_starts_a_fresh_claude() {
    let mut server = Server::start();
    let auto = AgentSettings {
        access: mux_core::Access::Auto,
        ..AgentSettings::default()
    };
    let id = server.claude_agent(auto);
    reply(&mut server, id, "first");
    configure(
        &mut server,
        id,
        AgentSettings {
            access: mux_core::Access::Supervised,
            ..AgentSettings::default()
        },
    );
    reply(&mut server, id, "second");
    let launches = server.claude_launches();
    assert_eq!(launches.len(), 2);
    let argv = launches[1]["argv"].to_string();
    assert!(argv.contains("\"--resume\""), "{argv}");
    assert!(argv.contains("\"manual\""), "{argv}");
}

#[test]
fn a_setting_claude_refuses_is_applied_by_relaunching() {
    let mut server = Server::start_with(&[("FAKE_CLAUDE_FAIL", "set_model")]);
    let id = server.claude_agent(AgentSettings::default());
    reply(&mut server, id, "first");
    configure(
        &mut server,
        id,
        AgentSettings {
            model: Some("fake-2".into()),
            ..AgentSettings::default()
        },
    );
    reply(&mut server, id, "second");
    reply(&mut server, id, "third");
    let launches = server.claude_launches();
    assert_eq!(launches.len(), 2, "relaunched once it refused");
    assert!(launches[1]["argv"].to_string().contains("\"fake-2\""));
}

#[test]
fn agents_forget_the_claude_session_mux_was_started_from() {
    let mut server = Server::start_with(&[
        ("CLAUDECODE", "1"),
        ("CLAUDE_CODE_ENTRYPOINT", "cli"),
        ("CLAUDE_CODE_SSE_PORT", "4242"),
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
    ]);
    let id = server.claude_agent(AgentSettings::default());
    reply(&mut server, id, "hello");
    for launch in server.claude_log().iter().filter(|e| e["argv"].is_array()) {
        assert_eq!(
            launch["env"],
            serde_json::json!(["CLAUDE_CODE_USE_BEDROCK"]),
            "the user's own settings stay"
        );
    }
}

fn continuations(server: &Server) -> usize {
    server
        .claude_stdin()
        .iter()
        .filter(|m| {
            m["type"] == "user"
                && m["message"]["content"][0]["text"]
                    .as_str()
                    .is_some_and(|t| t.contains("was cut off"))
        })
        .count()
}

fn wait_streaming(server: &mut Server) {
    server.wait("the reply to start", |s| {
        s.assistant_text().iter().any(|t| t.contains("more"))
    });
}

#[test]
fn a_turn_a_crash_cut_off_carries_on_in_the_same_conversation() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:slow");
    wait_streaming(&mut server);
    server.crash_and_restart();
    server.wait("the turn to carry on and finish", |s| {
        !s.busy && s.assistant_text().iter().any(|t| t.contains("was cut off"))
    });
    assert_eq!(continuations(&server), 1);
    let launches = server.claude_launches();
    assert_eq!(launches.len(), 2);
    assert!(
        launches[1]["argv"].to_string().contains("\"--resume\""),
        "it picks up claude's own conversation"
    );
}

#[test]
fn a_turn_that_keeps_crashing_the_server_stops_after_three_tries() {
    let mut server = Server::start_with(&[("FAKE_CLAUDE_STALL", "1")]);
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:slow");
    wait_streaming(&mut server);
    for _ in 0..3 {
        server.crash_and_restart();
        wait_streaming(&mut server);
    }
    server.crash_and_restart();
    server.wait("mux to give up", |s| !s.errors().is_empty());
    assert!(
        server.errors()[0].contains("3 times in a row"),
        "{:?}",
        server.errors()
    );
    assert_eq!(continuations(&server), 3);
    assert!(!server.busy);
}

#[test]
fn quitting_on_purpose_doesnt_carry_on_when_mux_starts_again() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:slow");
    wait_streaming(&mut server);
    server.quit_and_restart();
    server.wait("the transcript", |s| !s.items.is_empty());
    assert_eq!(continuations(&server), 0);
    assert_eq!(server.claude_launches().len(), 1);
}

#[test]
fn a_turn_a_usage_limit_stopped_carries_on_once_it_resets() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:limit");
    server.wait("the limit", |s| {
        s.errors().iter().any(|e| e.contains("carry on by itself"))
    });
    server.wait("the turn to carry on", |s| {
        !s.busy
            && s.assistant_text()
                .iter()
                .any(|t| t.contains("limit has reset"))
    });
    assert_eq!(
        server.claude_launches().len(),
        1,
        "the same claude carried on"
    );
}

#[test]
fn a_message_takes_over_from_waiting_for_a_limit() {
    let mut server = Server::start();
    let id = server.claude_agent(AgentSettings::default());
    server.say(id, "scenario:limit");
    server.wait("the limit", |s| {
        s.errors().iter().any(|e| e.contains("carry on by itself"))
    });
    reply(&mut server, id, "never mind");
    // past when it would have carried on
    std::thread::sleep(std::time::Duration::from_secs(7));
    server.wait("anything late", |_| true);
    let sent = server.claude_stdin();
    assert!(
        !sent
            .iter()
            .any(|m| m.to_string().contains("limit has reset")),
        "it waited for the user instead"
    );
}
