// the few things only the real claude can answer, run against the cli on this machine with the user's sign-in.
// each costs a few short prompts on the cheapest model, so they only run when asked for:
// cargo test -p mux-server --test live -- --ignored --test-threads 1
mod common;

use common::Server;
use mux_core::protocol::{AgentItem, AgentSettings, ClientMsg, QueueAction, ServerMsg};
use serde_json::Value;

fn haiku() -> AgentSettings {
    AgentSettings {
        model: Some("haiku".into()),
        ..AgentSettings::default()
    }
}

fn reply(server: &mut Server, id: u64, text: &str) {
    let replies = server.assistant_text().len();
    server.say(id, text);
    server.wait("the reply", |s| {
        !s.busy && s.assistant_text().len() > replies
    });
}

// what claude itself wrote down for the conversations in the test's project folder
fn claude_transcripts(server: &Server) -> Vec<Vec<Value>> {
    let project = server.dir.join("project");
    let name: String = project
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let folder = std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
        .join(".claude/projects")
        .join(name);
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .map(|e| {
            std::fs::read_to_string(e.path())
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .collect()
}

fn server_log(server: &Server) -> String {
    std::fs::read_to_string(server.dir.join("data/server.log")).unwrap_or_default()
}

#[test]
#[ignore = "runs the real claude"]
fn live_a_new_model_is_used_without_restarting_claude() {
    let mut server = Server::start_live();
    let id = server.claude_agent(haiku());
    reply(&mut server, id, "Reply with only the word one.");
    server.send(ClientMsg::AgentConfigure {
        id,
        settings: AgentSettings {
            provider: mux_core::Provider::Claude,
            model: Some("sonnet".into()),
            ..AgentSettings::default()
        },
    });
    reply(&mut server, id, "Reply with only the word two.");
    assert!(server.errors().is_empty(), "{:?}", server.errors());
    let log = server_log(&server);
    assert_eq!(log.matches("started Claude").count(), 1, "{log}");
    assert!(log.contains("changed claude's settings in place"), "{log}");
    let models: Vec<String> = claude_transcripts(&server)
        .concat()
        .iter()
        .filter(|e| e["type"] == "assistant")
        .filter_map(|e| e["message"]["model"].as_str().map(str::to_owned))
        .collect();
    assert!(
        models.first().is_some_and(|m| m.contains("haiku")),
        "{models:?}"
    );
    assert!(
        models.last().is_some_and(|m| m.contains("sonnet")),
        "{models:?}"
    );
}

#[test]
#[ignore = "runs the real claude"]
fn live_send_now_breaks_into_a_reply() {
    let mut server = Server::start_live();
    let id = server.claude_agent(haiku());
    server.say(id, "Write a 400 word story about a lighthouse keeper.");
    server.wait("the story to start", |s| {
        s.assistant_text().iter().any(|t| t.len() > 60)
    });
    server.say(id, "Stop the story. Reply with only the word PINEAPPLE.");
    server.wait("the message to queue", |s| {
        s.seen.iter().rev().find_map(|m| match m {
            ServerMsg::AgentQueue { queue, .. } => Some(!queue.is_empty()),
            _ => None,
        }) == Some(true)
    });
    server.send(ClientMsg::AgentQueue {
        id,
        index: 0,
        action: QueueAction::SendNow,
    });
    server.wait("the answer", |s| {
        !s.busy && s.assistant_text().iter().any(|t| t.contains("PINEAPPLE"))
    });
    assert!(server.errors().is_empty(), "{:?}", server.errors());
    // the story stopped short of what was asked for, since the message broke into it
    let story = &server.assistant_text()[0];
    assert!(story.split_whitespace().count() < 380, "{story}");
}

#[test]
#[ignore = "runs the real claude"]
fn live_a_turn_a_crash_cut_off_carries_on_in_the_same_conversation() {
    let mut server = Server::start_live();
    let id = server.claude_agent(haiku());
    server.say(
        id,
        "Count from 1 to 300, one number per line, and say nothing else.",
    );
    server.wait("the counting to start", |s| {
        s.assistant_text().iter().any(|t| t.lines().count() > 5)
    });
    server.crash_and_restart();
    server.wait("the turn to carry on and finish", |s| {
        let resumed = s.items.iter().position(
            |i| matches!(i, AgentItem::User { text, .. } if text.contains("was cut off")),
        );
        !s.busy
            && resumed.is_some_and(|at| {
                s.items[at..]
                    .iter()
                    .any(|i| matches!(i, AgentItem::Assistant { .. }))
            })
    });
    assert!(server.errors().is_empty(), "{:?}", server.errors());
    let transcripts = claude_transcripts(&server);
    let with_both = transcripts.iter().find(|t| {
        let text = serde_json::to_string(t).unwrap();
        text.contains("Count from 1 to 300") && text.contains("was cut off")
    });
    assert!(
        with_both.is_some(),
        "claude carried on in the conversation the count started in, across {} transcripts",
        transcripts.len()
    );
}
