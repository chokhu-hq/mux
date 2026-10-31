mod common;

use common::Server;
use mux_core::Provider;
use mux_core::protocol::{AgentItem, AgentSettings, ClientMsg, Decision, QueueAction, ServerMsg};

fn agent(server: &mut Server, provider: Provider) -> u64 {
    match provider {
        Provider::Claude => server.claude_agent(AgentSettings::default()),
        Provider::Codex => server.codex_agent(AgentSettings::default()),
        Provider::OpenCode => unreachable!(),
    }
}

fn wait_reply(server: &mut Server, text: &str) {
    server.wait("the expected reply to finish", |s| {
        !s.busy && s.assistant_text().iter().any(|reply| reply == text)
    });
}

fn wait_queue(server: &mut Server, texts: &[&str], held: bool) {
    server.wait_for("the expected queue", |msg| match msg {
        ServerMsg::AgentQueue { queue, held: h, .. }
            if *h == held && queue.iter().map(|q| q.text.as_str()).collect::<Vec<_>>() == texts =>
        {
            Some(())
        }
        _ => None,
    });
}

#[test]
fn stop_and_resend_during_thread_start_or_resume_sends_only_the_replacement() {
    for method in ["thread/start", "thread/resume"] {
        for resend_while_paused in [false, true] {
            let mut server = Server::start();
            let id = server.codex_agent(AgentSettings::default());
            if method == "thread/resume" {
                server.say(id, "warm up");
                wait_reply(&mut server, "ok: warm up");
                server.quit_and_restart();
            }
            server.hold_codex(method);
            server.say(id, "cancel me");
            server.wait_codex_held(method);
            server.wait("startup to be busy", |s| s.busy);
            server.send(ClientMsg::AgentStop { id });
            server.wait("startup to stop", |s| !s.busy);

            let settings = AgentSettings {
                provider: Provider::Codex,
                model: Some("replacement-model".into()),
                plan: true,
                ..AgentSettings::default()
            };
            server.send(ClientMsg::AgentConfigure { id, settings });
            server.wait_for("the replacement settings", |msg| match msg {
                ServerMsg::AgentSettings { settings, .. }
                    if settings.model.as_deref() == Some("replacement-model") =>
                {
                    Some(())
                }
                _ => None,
            });
            if resend_while_paused {
                server.say(id, "replacement");
                server.wait("the replacement to be busy", |s| s.busy);
            }
            server.release_codex();
            if !resend_while_paused {
                server.say(id, "replacement");
            }
            wait_reply(&mut server, "ok: replacement");
            let turns = server.codex_calls("turn/start");
            assert!(
                turns
                    .iter()
                    .all(|turn| turn["params"]["input"][0]["text"] != "cancel me")
            );
            assert_eq!(turns.len(), if method == "thread/resume" { 2 } else { 1 });
            let params = &turns.last().unwrap()["params"];
            assert_eq!(params["model"], "replacement-model");
            assert_eq!(params["collaborationMode"]["mode"], "plan");
            assert!(server.errors().is_empty(), "{:?}", server.errors());
        }
    }
}

#[test]
fn stop_before_the_turn_id_holds_the_queue_in_either_reply_order() {
    for notify_first in [false, true] {
        let env = if notify_first {
            vec![("FAKE_CODEX_NOTIFY_FIRST", "1")]
        } else {
            Vec::new()
        };
        let mut server = Server::start_with(&env);
        let id = server.codex_agent(AgentSettings::default());
        server.hold_codex("turn/start");
        server.say(id, "scenario:slow");
        server.wait_codex_held("turn/start");
        server.say(id, "after stop");
        wait_queue(&mut server, &["after stop"], false);
        server.send(ClientMsg::AgentStop { id });
        wait_queue(&mut server, &["after stop"], true);
        server.release_codex();
        server.wait("the cancelled turn to end", |s| !s.busy);
        assert_eq!(server.codex_calls("turn/interrupt").len(), 1);
        assert_eq!(server.codex_calls("turn/start").len(), 1);
        server.send(ClientMsg::AgentResume { id });
        wait_reply(&mut server, "ok: after stop");
        assert_eq!(server.codex_calls("turn/start").len(), 2);
        assert!(server.errors().is_empty(), "{:?}", server.errors());
    }
}

#[test]
fn a_crash_during_startup_does_not_repeat_the_abandoned_message() {
    let mut server = Server::start();
    let id = server.codex_agent(AgentSettings::default());
    server.hold_codex("thread/start");
    server.say(id, "abandoned");
    server.wait_codex_held("thread/start");
    server.crash_held_codex();
    server.wait("the startup failure", |s| !s.busy && !s.errors().is_empty());
    server.say(id, "replacement");
    wait_reply(&mut server, "ok: replacement");
    let turns = server.codex_calls("turn/start");
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["params"]["input"][0]["text"], "replacement");
}

#[test]
fn reconnecting_during_an_approval_keeps_the_same_turn_and_question() {
    for provider in [Provider::Claude, Provider::Codex] {
        let mut server = Server::start();
        let id = agent(&mut server, provider);
        server.say(id, "scenario:approve");
        let approval = server.wait_for("the approval", |msg| match msg {
            ServerMsg::AgentItem {
                item:
                    AgentItem::Approval {
                        id, decision: None, ..
                    },
                ..
            } => Some(*id),
            _ => None,
        });
        let pid = server.pid();
        server.reconnect();
        server.wait_for("the pending approval after reconnecting", |msg| match msg {
            ServerMsg::AgentTranscript {
                busy: true, items, ..
            } if items.iter().any(|item| {
                matches!(item,
                    AgentItem::Approval { id, decision: None, .. } if *id == approval
                )
            }) =>
            {
                Some(())
            }
            _ => None,
        });
        assert_eq!(server.pid(), pid);
        server.send(ClientMsg::AgentDecide {
            id,
            approval,
            decision: Decision::Allow,
        });
        server.wait_idle();
        assert_eq!(server.assistant_text().len(), 1);
        let user_messages = match provider {
            Provider::Codex => server.codex_calls("turn/start").len(),
            Provider::Claude => server
                .claude_stdin()
                .iter()
                .filter(|msg| msg["type"] == "user")
                .count(),
            Provider::OpenCode => unreachable!(),
        };
        assert_eq!(user_messages, 1);
        assert!(server.items.iter().any(|item| matches!(item,
            AgentItem::Approval { id, decision: Some(Decision::Allow), .. } if *id == approval
        )));
        assert!(server.errors().is_empty(), "{:?}", server.errors());
    }
}

#[test]
fn a_stopped_queue_survives_restart_edit_and_removal_before_resuming() {
    for provider in [Provider::Claude, Provider::Codex] {
        let mut server = Server::start();
        let id = agent(&mut server, provider);
        server.say(id, "scenario:slow");
        server.wait("the reply to start", |s| {
            s.assistant_text().iter().any(|t| t.contains("more"))
        });
        server.say(id, "edit me");
        server.say(id, "remove me");
        wait_queue(&mut server, &["edit me", "remove me"], false);
        server.send(ClientMsg::AgentStop { id });
        server.wait("the turn to stop", |s| !s.busy);
        server.quit_and_restart();
        wait_queue(&mut server, &["edit me", "remove me"], true);
        server.send(ClientMsg::AgentQueue {
            id,
            index: 0,
            action: QueueAction::Edit("edited".into()),
        });
        wait_queue(&mut server, &["edited", "remove me"], true);
        server.send(ClientMsg::AgentQueue {
            id,
            index: 1,
            action: QueueAction::Remove,
        });
        wait_queue(&mut server, &["edited"], true);
        server.quit_and_restart();
        wait_queue(&mut server, &["edited"], true);
        assert!(!server.busy);
        server.send(ClientMsg::AgentResume { id });
        wait_reply(&mut server, "ok: edited");
        assert!(server.errors().is_empty(), "{:?}", server.errors());
        assert_eq!(
            server
                .items
                .iter()
                .filter(|item| matches!(item, AgentItem::User { .. }))
                .count(),
            2
        );
    }
}
