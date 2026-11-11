use super::sidebar::{group_gap, most_pressing};
use super::transcript::{chat_items, mcp_tool, shell_body, tool_kind};
use super::*;

#[test]
fn mcp_tools_read_as_server_and_tool() {
    assert_eq!(
        mcp_tool("mcp__blender__get_scene").as_deref(),
        Some("blender › get_scene")
    );
    assert_eq!(mcp_tool("Bash"), None);
    assert_eq!(tool_kind("mcp__blender__get_scene"), ToolKind::Mcp);
}

#[test]
fn old_limit_messages_become_one_notice_and_real_errors_stay_errors() {
    let transcript = vec![
        AgentItem::Assistant {
            text: "Weekly limit reached".into(),
        },
        AgentItem::Error {
            message: "Weekly limit reached".into(),
        },
        AgentItem::Error {
            message: "Mux will carry on by itself in about 34 h, once the limit resets.".into(),
        },
        AgentItem::Error {
            message: "Connection failed".into(),
        },
    ];
    let (items, approvals) = chat_items(&transcript, false, Provider::Claude);
    assert_eq!(items.len(), 2);
    assert_eq!(approvals.len(), 2);
    assert!(matches!(&items[0], ChatItem::UsageLimit { message, .. }
        if message.matches("Weekly limit reached").count() == 1 && message.contains("34 h")));
    assert!(matches!(&items[1], ChatItem::Error(text) if text == "Connection failed"));
}

#[test]
fn sending_another_message_hides_the_old_automatic_retry_status() {
    let transcript = vec![
        AgentItem::UsageLimit {
            message: "Limit reached".into(),
            retry_at: Some(100),
        },
        AgentItem::User {
            text: "try again".into(),
            attachments: Vec::new(),
            queued: false,
            rewind: None,
            took: None,
        },
    ];
    let (items, _) = chat_items(&transcript, false, Provider::Claude);
    assert!(matches!(
        &items[0],
        ChatItem::UsageLimit { retry_at: None, .. }
    ));
}

#[test]
fn the_default_model_says_which_model_it_is() {
    let mut default = ModelOption {
        id: "default".into(),
        name: "Default (recommended)".into(),
        description: "Opus 5.5 · Best for everyday, complex tasks".into(),
        efforts: Vec::new(),
        default_effort: String::new(),
        is_default: true,
        fast: None,
    };
    name_default(&mut default);
    assert_eq!(default.name, "Default (Opus 5.5)");
    name_default(&mut default);
    assert_eq!(
        default.name, "Default (Opus 5.5)",
        "naming it twice changes nothing"
    );
}

#[test]
fn new_agents_start_with_the_defaults_from_settings() {
    let mut sessions = Sessions {
        agent_defaults: mux_core::protocol::AgentSettings {
            provider: Provider::Claude,
            model: Some("opus".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    sessions.create("New agent", SessionKind::Agent);
    sessions.create("Shell", SessionKind::Shell);
    sessions.outbox.clear();
    sessions.apply(ServerMsg::Created { id: 7 });
    sessions.apply(ServerMsg::Created { id: 8 });
    // only the agent is configured, and a shell confirmed after it doesn't take its settings
    assert_eq!(sessions.outbox.len(), 1);
    assert!(matches!(
        &sessions.outbox[0],
        ClientMsg::AgentConfigure { id: 7, settings }
            if settings.provider == Provider::Claude
                && settings.model.as_deref() == Some("opus")
                && settings.access == mux_core::Access::Auto
    ));
}

#[test]
fn finished_turns_that_touch_several_files_get_a_summary() {
    let user = || AgentItem::User {
        text: "go".into(),
        attachments: Vec::new(),
        queued: false,
        rewind: None,
        took: None,
    };
    let edit = |path: &str| AgentItem::FileChange {
        changes: vec![wire::FileChange {
            path: path.into(),
            diff: "@@ -1 +1 @@\n-a\n+b\n+c".into(),
        }],
        status: ItemStatus::Done,
    };
    let transcript = vec![
        user(),
        edit("a.rs"),
        edit("b.rs"),
        edit("a.rs"),
        user(),
        edit("c.rs"),
    ];
    let (items, approvals) = chat_items(&transcript, false, Provider::Codex);
    assert_eq!(items.len(), approvals.len());
    let summaries: Vec<(usize, usize, usize, usize)> = items
        .iter()
        .filter_map(|i| match i {
            ChatItem::Changes(c) => Some((c.files, c.added, c.removed, c.first)),
            _ => None,
        })
        .collect();
    // the second turn only touched one file, so its edit card says it all
    assert_eq!(summaries, [(2, 6, 3, 1)]);
    let (busy, _) = chat_items(&transcript[..4], true, Provider::Codex);
    assert!(!busy.iter().any(|i| matches!(i, ChatItem::Changes(_))));
}

#[test]
fn a_finish_is_announced_only_once_it_lasts() {
    let state = |status| ServerMsg::State {
        groups: vec![GroupInfo {
            id: 1,
            name: "p".into(),
            path: None,
            branch: None,
            open: true,
            sessions: vec![SessionInfo {
                id: 2,
                title: "fix tests".into(),
                kind: SessionKind::Agent,
                status,
                meta: String::new(),
                updated: 0,
                provider: Provider::Claude,
                worktree: None,
                branch: None,
                summary: None,
            }],
        }],
    };
    let mut sessions = Sessions::default();
    sessions.apply(state(SessionStatus::Running));
    sessions.apply(state(SessionStatus::Done));
    assert!(
        sessions.take_alerts(None, 10.0).is_empty(),
        "not straight away"
    );
    assert!(sessions.take_alerts(None, 11.0).is_empty());
    assert!(!sessions.unread.contains(&2), "and no dot yet either");
    // it carried on by itself before the moment was up
    sessions.apply(state(SessionStatus::Running));
    assert!(sessions.take_alerts(None, 20.0).is_empty());
    assert!(!sessions.alert_due());

    sessions.apply(state(SessionStatus::Done));
    assert!(sessions.take_alerts(None, 30.0).is_empty());
    let alerts = sessions.take_alerts(None, 33.5);
    assert!(matches!(
        alerts.as_slice(),
        [Alert {
            id: 2,
            event: Event::Finished,
            ..
        }]
    ));
    assert!(sessions.unread.contains(&2));
}

#[test]
fn finished_agents_alert_once_and_stay_unread_until_looked_at() {
    let state = |status| ServerMsg::State {
        groups: vec![GroupInfo {
            id: 1,
            name: "p".into(),
            path: None,
            branch: None,
            open: true,
            sessions: vec![SessionInfo {
                id: 2,
                title: "fix tests".into(),
                kind: SessionKind::Agent,
                status,
                meta: String::new(),
                updated: 0,
                provider: Provider::Claude,
                worktree: None,
                branch: None,
                summary: None,
            }],
        }],
    };
    let mut sessions = Sessions::default();
    // what's already done at startup isn't news
    sessions.apply(state(SessionStatus::Done));
    assert!(sessions.take_alerts(None, 0.0).is_empty());
    sessions.apply(state(SessionStatus::Running));
    sessions.apply(state(SessionStatus::Waiting));
    let alerts = sessions.take_alerts(None, 0.0);
    assert!(matches!(
        alerts.as_slice(),
        [Alert {
            id: 2,
            event: Event::Waiting,
            ..
        }]
    ));
    assert!(sessions.unread.contains(&2));
    sessions.take_alerts(Some(2), 0.0);
    assert!(!sessions.unread.contains(&2));
}

#[test]
fn streamed_text_joins_the_reply_once_and_in_place() {
    let mut sessions = Sessions::default();
    sessions.locals.insert(5, Local::default());
    sessions.apply(ServerMsg::AgentTranscript {
        id: 5,
        items: vec![AgentItem::Assistant { text: "Hel".into() }],
        busy: true,
        settings: AgentSettings::default(),
        context: None,
    });
    let append = |at: usize, text: &str| ServerMsg::AgentAppend {
        id: 5,
        index: 0,
        at,
        text: text.into(),
    };
    sessions.apply(append(3, "lo"));
    // the same piece again, from before a full copy of the reply was sent
    sessions.apply(append(3, "lo"));
    sessions.apply(append(99, "?"));
    assert_eq!(
        sessions.locals[&5].transcript,
        [AgentItem::Assistant {
            text: "Hello".into()
        }]
    );
}

#[test]
fn a_closed_project_shows_what_most_needs_looking_at() {
    let session = |id, kind, status| SessionInfo {
        id,
        title: String::new(),
        kind,
        status,
        meta: String::new(),
        updated: 0,
        provider: Provider::Claude,
        worktree: None,
        branch: None,
        summary: None,
    };
    let shell = session(1, SessionKind::Shell, SessionStatus::Running);
    let done = session(2, SessionKind::Agent, SessionStatus::Done);
    let failed = session(3, SessionKind::Agent, SessionStatus::Failed);
    let working = session(4, SessionKind::Agent, SessionStatus::Running);
    let waiting = session(5, SessionKind::Agent, SessionStatus::Waiting);
    let mut unread = HashSet::new();
    assert_eq!(
        most_pressing(std::slice::from_ref(&shell), &unread),
        None,
        "an open shell is nothing new"
    );
    assert_eq!(
        most_pressing(&[done.clone(), failed.clone()], &unread),
        None,
        "already seen"
    );
    unread.extend([2, 3]);
    assert_eq!(
        most_pressing(&[done.clone(), working.clone()], &unread),
        Some(SessionStatus::Running)
    );
    assert_eq!(
        most_pressing(&[done.clone(), working.clone(), failed.clone()], &unread),
        Some(SessionStatus::Failed)
    );
    assert_eq!(
        most_pressing(&[shell, done, failed, working, waiting], &unread),
        Some(SessionStatus::Waiting)
    );
}

#[test]
fn a_reset_is_offered_for_codex_and_says_what_happened() {
    let mut sessions = Sessions::default();
    sessions.apply(ServerMsg::AgentResets {
        provider: Provider::Codex,
        resets: vec![
            wire::LimitReset {
                title: "Full reset".into(),
                expires_at: None,
            };
            2
        ],
    });
    assert!(
        sessions.reset_offer(Provider::Claude).is_none(),
        "claude has no resets"
    );
    assert_eq!(sessions.reset_offer(Provider::Codex).unwrap().left, 2);
    sessions.use_reset(Provider::Codex);
    assert!(sessions.reset_offer(Provider::Codex).unwrap().pending);
    assert!(matches!(
        sessions.outbox.last(),
        Some(ClientMsg::UseReset {
            provider: Provider::Codex
        })
    ));
    sessions.apply(ServerMsg::ResetResult {
        provider: Provider::Codex,
        result: wire::ResetResult::NothingToReset,
    });
    let offer = sessions.reset_offer(Provider::Codex).unwrap();
    assert!(!offer.pending);
    assert!(offer.note.unwrap().contains("kept"));
}

#[test]
fn a_dragged_group_lands_in_the_gap_under_the_pointer() {
    let blocks = [(0.0, 40.0), (46.0, 200.0), (206.0, 240.0)];
    // dragging the first group past the middle of the second puts it after the second
    assert_eq!(group_gap(&blocks, 130.0, Some(0)), Some(2));
    assert_eq!(group_gap(&blocks, 300.0, Some(0)), Some(3));
    // its own gaps above and below leave it where it is
    assert_eq!(group_gap(&blocks, 10.0, Some(0)), None);
    assert_eq!(group_gap(&blocks, 100.0, Some(0)), None);
    assert_eq!(group_gap(&blocks, 10.0, Some(2)), Some(0));
}

#[test]
fn unwraps_shell_commands() {
    assert_eq!(
        shell_body("/usr/bin/bash -lc \"printf 'hi\\\\n' > hello.txt\""),
        "printf 'hi\\n' > hello.txt"
    );
    assert_eq!(shell_body("/bin/zsh -c 'ls -la'"), "ls -la");
    assert_eq!(shell_body("cargo test -c foo"), "cargo test -c foo");
}

#[test]
fn approvals_line_up_with_chat_items() {
    let transcript = vec![
        AgentItem::User {
            text: "hi".into(),
            attachments: Vec::new(),
            queued: false,
            rewind: None,
            took: None,
        },
        AgentItem::FileChange {
            changes: vec![
                wire::FileChange {
                    path: "a".into(),
                    diff: "@@ -1 +1 @@\n-old\n+new".into(),
                },
                wire::FileChange {
                    path: "b".into(),
                    diff: "fresh file".into(),
                },
            ],
            status: ItemStatus::Done,
        },
        AgentItem::Approval {
            id: 9,
            title: "Run?".into(),
            detail: String::new(),
            decision: None,
        },
    ];
    let (items, approvals) = chat_items(&transcript, true, Provider::Codex);
    // no thinking indicator while the approval waits on the user
    assert_eq!(items.len(), 4, "user, two diffs, approval");
    assert_eq!(approvals, vec![None, None, None, Some(9)]);
    let ChatItem::Diff(first) = &items[1] else {
        panic!()
    };
    assert_eq!(first.lines.len(), 2);
    assert_eq!(first.lines[0].kind, DiffKind::Remove);
    let ChatItem::Diff(second) = &items[2] else {
        panic!()
    };
    assert_eq!(second.lines[0].kind, DiffKind::Add);
}

// a long chat: 200 turns of ten capped tool outputs, an edit and a reply, about 50 MB
fn big_chat(turns: usize) -> Vec<AgentItem> {
    let mut items = Vec::new();
    for turn in 0..turns {
        items.push(AgentItem::User {
            text: format!("message {turn}"),
            attachments: Vec::new(),
            queued: false,
            rewind: None,
            took: None,
        });
        for _ in 0..10 {
            items.push(AgentItem::Command {
                command: "cargo test --workspace".into(),
                output: "test result: ok\n".repeat(1600),
                exit_code: Some(0),
                status: ItemStatus::Done,
                background: false,
            });
        }
        items.push(AgentItem::FileChange {
            changes: vec![wire::FileChange {
                path: "/tmp/src/lib.rs".into(),
                diff: "@@ -1 +1 @@\n".to_owned() + &"+let a = 1;\n".repeat(200),
            }],
            status: ItemStatus::Done,
        });
        items.push(AgentItem::Assistant {
            text: "Fixed it. Here's **what** changed:\n\n- one\n- two\n\n".repeat(20),
        });
    }
    items
}

// run with: cargo test -p mux-gui --release bench_ -- --ignored --nocapture
#[test]
#[ignore]
fn bench_updates_in_a_long_chat() {
    let id = 5;
    let mut sessions = Sessions::default();
    sessions.locals.insert(id, Local::default());
    let items = big_chat(200);
    let size: usize = items
        .iter()
        .map(|item| mux_core::protocol::encode(item).unwrap().len())
        .sum();
    let start = std::time::Instant::now();
    sessions.apply(ServerMsg::AgentTranscript {
        id,
        items,
        busy: true,
        settings: AgentSettings::default(),
        context: None,
    });
    println!("opening {} MB: {:?}", size / (1024 * 1024), start.elapsed());
    let start = std::time::Instant::now();
    let last = sessions.locals[&id].transcript.len() - 1;
    let mut at = match &sessions.locals[&id].transcript[last] {
        AgentItem::Assistant { text } => text.len(),
        _ => unreachable!(),
    };
    for _ in 0..100 {
        sessions.apply(ServerMsg::AgentAppend {
            id,
            index: last,
            at,
            text: "word ".into(),
        });
        at += 5;
    }
    println!("a piece of streamed text: {:?}", start.elapsed() / 100);
    let start = std::time::Instant::now();
    for _ in 0..20 {
        sessions.apply(ServerMsg::AgentItem {
            id,
            index: last + 1,
            item: AgentItem::Command {
                command: "ls".into(),
                output: String::new(),
                exit_code: None,
                status: ItemStatus::Running,
                background: false,
            },
        });
    }
    println!("a new tool call: {:?}", start.elapsed() / 20);

    // one long turn, where there's no earlier message to stop at
    let mut items = big_chat(200);
    items.retain(|item| !matches!(item, AgentItem::User { .. }));
    items.insert(0, big_chat(1).remove(0));
    items.push(AgentItem::Assistant { text: "Hi".into() });
    let last = items.len() - 1;
    sessions.apply(ServerMsg::AgentTranscript {
        id,
        items,
        busy: true,
        settings: AgentSettings::default(),
        context: None,
    });
    let start = std::time::Instant::now();
    for i in 0..100 {
        sessions.apply(ServerMsg::AgentAppend {
            id,
            index: last,
            at: 2 + i * 5,
            text: "word ".into(),
        });
    }
    println!(
        "streamed text in one long turn: {:?}",
        start.elapsed() / 100
    );
    let start = std::time::Instant::now();
    for _ in 0..20 {
        sessions.apply(ServerMsg::AgentItem {
            id,
            index: last + 1,
            item: AgentItem::Command {
                command: "ls".into(),
                output: String::new(),
                exit_code: None,
                status: ItemStatus::Running,
                background: false,
            },
        });
    }
    println!(
        "a new tool call in one long turn: {:?}",
        start.elapsed() / 20
    );
}

// redoing only the latest turn has to end up exactly where building the whole chat does
#[test]
fn updating_the_latest_turn_matches_building_the_whole_chat() {
    let user = |queued| AgentItem::User {
        text: "go".into(),
        attachments: Vec::new(),
        queued,
        rewind: None,
        took: None,
    };
    let edit = |path: &str| AgentItem::FileChange {
        changes: vec![wire::FileChange {
            path: path.into(),
            diff: "@@ -1 +1 @@\n-a\n+b\n".into(),
        }],
        status: ItemStatus::Done,
    };
    let kinds = |pick: u64| match pick % 11 {
        0 | 1 => user(pick % 22 == 1),
        2 => AgentItem::Assistant {
            text: String::new(),
        },
        3 => AgentItem::Assistant {
            text: "done".into(),
        },
        4 => AgentItem::Reasoning {
            text: "hm".into(),
            started: 1,
            ended: None,
        },
        5 => edit("a"),
        6 => edit("b"),
        7 => AgentItem::Plan {
            text: "plan".into(),
        },
        8 => AgentItem::UsageLimit {
            message: "limit".into(),
            retry_at: Some(9),
        },
        9 => AgentItem::Approval {
            id: pick,
            title: "ok?".into(),
            detail: String::new(),
            decision: pick.is_multiple_of(2).then_some(wire::Decision::Allow),
        },
        _ => AgentItem::Error {
            message: "oops".into(),
        },
    };
    let mut sessions = Sessions::default();
    sessions.locals.insert(1, Local::default());
    let mut seed = 7u64;
    let mut roll = move || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed >> 33
    };
    for _ in 0..3000 {
        let (pick, len) = (roll(), sessions.locals[&1].transcript.len());
        let msg = match roll() % 10 {
            // a change to something already there, sometimes well before the latest message
            0 | 1 if len > 0 => ServerMsg::AgentItem {
                id: 1,
                index: roll() as usize % len,
                item: kinds(pick),
            },
            2 => ServerMsg::AgentBusy {
                id: 1,
                busy: pick.is_multiple_of(2),
            },
            3 | 4 if len > 0 => {
                let index = if pick.is_multiple_of(4) {
                    roll() as usize % len
                } else {
                    len - 1
                };
                let mut item = sessions.locals[&1].transcript[index].clone();
                match item.streamed_text() {
                    Some(text) => ServerMsg::AgentAppend {
                        id: 1,
                        index,
                        at: text.len(),
                        text: if pick.is_multiple_of(3) {
                            " ".into()
                        } else {
                            "word".into()
                        },
                    },
                    None => ServerMsg::AgentBusy { id: 1, busy: true },
                }
            }
            _ => ServerMsg::AgentItem {
                id: 1,
                index: len,
                item: kinds(pick),
            },
        };
        sessions.apply(msg);
        let local = &sessions.locals[&1];
        let (whole, approvals) = chat_items(&local.transcript, local.busy, local.settings.provider);
        assert_eq!(format!("{:?}", local.chat), format!("{whole:?}"));
        assert_eq!(local.approvals, approvals);
    }
}
