use super::claude::claude_failure;
use super::*;
use mux_core::protocol::TodoStatus;

fn agent() -> Agent {
    Agent::load(Path::new("/nonexistent/mux-agent.json"))
}

#[test]
fn stopping_before_initialization_prevents_the_queued_turn() {
    for provider in [Provider::Codex, Provider::OpenCode] {
        let mut agent = agent();
        agent.settings.provider = provider;
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.record("cancel me", &[], false);
        agent.transmit("cancel me".into(), Vec::new(), false);
        agent.calls.insert(0, Call::Initialize);
        agent.next_call = 1;
        agent.stop();
        assert!(!agent.busy);
        agent.handle(json!({ "id": 0, "result": {} }));
        while rx.try_recv().is_ok() {}
        agent.handle(json!({ "id": 1, "result": {
            "sessionId": "s", "thread": { "id": "t" }
        } }));
        assert!(rx.try_recv().is_err());
        assert!(agent.queued.is_empty());
        assert!(agent.thread_live);
        assert!(!agent.busy);
        agent.record("try again", &[], false);
        agent.transmit("try again".into(), Vec::new(), false);
        let sent: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(
            sent["method"],
            match provider {
                Provider::Codex => "turn/start",
                Provider::OpenCode => "session/prompt",
                Provider::Claude => unreachable!(),
            }
        );
    }
}

#[test]
fn codex_stop_waits_for_the_turn_id() {
    for notification_first in [false, true] {
        let mut agent = agent();
        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.thread_id = Some("t".into());
        agent.thread_live = true;
        agent.record("go", &[], false);
        agent.transmit("go".into(), Vec::new(), false);
        let start: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        agent.stop();
        assert!(rx.try_recv().is_err());
        let response = json!({ "id": start["id"], "result": { "turn": { "id": "turn" } } });
        let notification =
            json!({ "method": "turn/started", "params": { "turn": { "id": "turn" } } });
        let messages = if notification_first {
            [notification, response]
        } else {
            [response, notification]
        };
        for message in messages {
            agent.handle(message);
        }
        let interrupt: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        assert_eq!(interrupt["method"], "turn/interrupt");
        assert_eq!(interrupt["params"]["turnId"], "turn");
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn an_exit_during_startup_does_not_replay_the_abandoned_prompt() {
    for provider in [Provider::Codex, Provider::OpenCode] {
        let mut agent = agent();
        agent.settings.provider = provider;
        let (tx, _) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.record("abandoned", &[], false);
        agent.transmit("abandoned".into(), Vec::new(), false);
        agent.exited();
        assert!(!agent.busy);

        let (tx, mut rx) = mpsc::unbounded_channel();
        agent.process = Some(Process::fake(tx));
        agent.record("replacement", &[], false);
        agent.transmit("replacement".into(), Vec::new(), false);
        agent.calls.insert(0, Call::ResumeThread);
        agent.next_call = 1;
        agent.handle(json!({ "id": 0, "result": {
            "thread": { "id": "thread" }, "sessionId": "session"
        } }));
        let prompt: Value = serde_json::from_str(&rx.try_recv().unwrap()).unwrap();
        let text = match provider {
            Provider::Codex => &prompt["params"]["input"][0]["text"],
            Provider::OpenCode => &prompt["params"]["prompt"][0]["text"],
            Provider::Claude => unreachable!(),
        };
        assert_eq!(text, "replacement");
        assert!(rx.try_recv().is_err());
    }
}

#[test]
fn stopping_compaction_while_resuming_prevents_the_compaction_request() {
    for provider in [Provider::Codex, Provider::OpenCode] {
        for initialized in [false, true] {
            let mut agent = agent();
            agent.settings.provider = provider;
            agent.thread_id = Some("thread".into());
            agent.session_id = Some("session".into());
            let (tx, mut rx) = mpsc::unbounded_channel();
            agent.process = Some(Process::fake(tx));
            agent.calls.insert(0, Call::Initialize);
            agent.next_call = 1;
            let (events, _) = mpsc::unbounded_channel();
            agent.compact(1, 1, PathBuf::from("/project"), &events);
            if initialized {
                agent.handle(json!({ "id": 0, "result": {} }));
            }
            agent.stop();
            if !initialized {
                agent.handle(json!({ "id": 0, "result": {} }));
            }
            while rx.try_recv().is_ok() {}
            agent.handle(json!({ "id": 1, "result": {
                "thread": { "id": "thread" }, "sessionId": "session"
            } }));
            assert!(!agent.busy);
            assert!(matches!(
                agent.items.last(),
                Some(AgentItem::Compaction {
                    status: ItemStatus::Failed
                })
            ));
            assert!(rx.try_recv().is_err());
        }
    }
}

#[test]
fn a_stopped_claude_turn_is_not_an_error() {
    let aborted = json!({
        "type": "result",
        "subtype": "error_during_execution",
        "is_error": true,
        "result": null,
        "errors": ["[ede_diagnostic] result_type=user last_content_type=n/a stop_reason=tool_use"],
        "terminal_reason": "aborted_tools",
    });
    assert_eq!(claude_failure(&aborted, false), None);
    let mut unexplained = aborted.clone();
    unexplained["terminal_reason"] = json!("completed");
    assert_eq!(claude_failure(&unexplained, true), None);
    assert!(
        claude_failure(&unexplained, false)
            .unwrap()
            .contains("server log")
    );
    let login = json!({
        "subtype": "success",
        "is_error": true,
        "result": "Invalid API key · Please run /login",
    });
    assert_eq!(
        claude_failure(&login, false).as_deref(),
        Some("Invalid API key · Please run /login")
    );
    let turns = json!({ "subtype": "error_max_turns", "is_error": true });
    assert!(
        claude_failure(&turns, false)
            .unwrap()
            .contains("turn limit")
    );
    let fine = json!({ "subtype": "success", "is_error": false, "result": "done" });
    assert_eq!(claude_failure(&fine, false), None);
}

fn note(agent: &mut Agent, method: &str, params: Value) {
    agent.handle(json!({ "method": method, "params": params }));
}

// codex 0.x with a summary asked for: a reasoning item, its streamed summary, then the finished one
#[test]
fn codex_reasoning_streams_then_takes_the_finished_summary() {
    let mut agent = agent();
    let item = json!({ "type": "reasoning", "id": "rs_1", "summary": [], "content": [] });
    note(&mut agent, "item/started", json!({ "item": item }));
    note(
        &mut agent,
        "item/reasoning/summaryTextDelta",
        json!({ "itemId": "rs_1", "delta": "**Counting**" }),
    );
    note(
        &mut agent,
        "item/reasoning/summaryPartAdded",
        json!({ "itemId": "rs_1", "summaryIndex": 1 }),
    );
    note(
        &mut agent,
        "item/reasoning/summaryTextDelta",
        json!({ "itemId": "rs_1", "delta": "Then check." }),
    );
    assert!(
        matches!(&agent.items[0], AgentItem::Reasoning { text, ended: None, .. } if text == "**Counting**\n\nThen check.")
    );
    let done = json!({ "type": "reasoning", "id": "rs_1", "summary": ["**Counting**", "Then check twice."], "content": [] });
    note(&mut agent, "item/completed", json!({ "item": done }));
    assert!(
        matches!(&agent.items[0], AgentItem::Reasoning { text, ended: Some(_), .. } if text == "**Counting**\n\nThen check twice.")
    );
}

// claude streams a thinking block, often with no words in it, then its reply
#[test]
fn claude_thinking_ends_when_the_reply_starts() {
    let mut agent = claude_agent();
    let event = |e: Value| json!({ "type": "stream_event", "event": e });
    agent.handle(event(json!({ "type": "content_block_start", "content_block": { "type": "thinking", "thinking": "" } })));
    agent.handle(event(json!({ "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": "" } })));
    assert!(matches!(
        &agent.items[0],
        AgentItem::Reasoning { ended: None, .. }
    ));
    agent.handle(event(
        json!({ "type": "content_block_start", "content_block": { "type": "text" } }),
    ));
    assert!(
        matches!(&agent.items[0], AgentItem::Reasoning { text, ended: Some(_), .. } if text.is_empty())
    );
    assert!(matches!(&agent.items[1], AgentItem::Assistant { .. }));
}

// the order claude 2.1.312 sent things in when two messages arrived during a turn and were answered together
#[test]
fn a_claude_error_said_as_a_reply_shows_once() {
    let mut agent = claude_agent();
    agent.set_busy(true);
    let said = "There's an issue with the selected model.";
    agent.handle(json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": said }] } }));
    agent.handle(
        json!({ "type": "result", "subtype": "success", "is_error": true, "result": said }),
    );
    assert_eq!(
        agent.items,
        [AgentItem::Error {
            message: said.into()
        }]
    );
}

#[test]
fn claude_is_done_when_merged_messages_get_one_answer() {
    let mut agent = claude_agent();
    let user = |text: &str| AgentItem::User {
        text: text.into(),
        attachments: Vec::new(),
        queued: false,
        rewind: None,
        took: None,
    };
    agent.items = vec![user("count")];
    agent.claude_waiting = VecDeque::from(["count".to_owned()]);
    agent.set_busy(true);
    let echo = |text: &str| json!({ "type": "user", "isReplay": true, "message": { "role": "user", "content": text } });
    let result = json!({ "type": "result", "subtype": "success", "result": "ok" });
    agent.handle(echo("count"));
    // two more arrive while it counts
    for text in ["Now say hi.", "And say bye."] {
        agent.items.push(AgentItem::User {
            text: text.into(),
            attachments: Vec::new(),
            queued: true,
            rewind: None,
            took: None,
        });
        agent.claude_waiting.push_back(text.into());
    }
    agent.handle(result.clone());
    assert!(agent.busy, "two messages are still waiting");
    agent.handle(echo("Now say hi.\nAnd say bye."));
    assert!(agent.claude_waiting.is_empty());
    assert!(
        !agent
            .items
            .iter()
            .any(|i| matches!(i, AgentItem::User { queued: true, .. }))
    );
    agent.handle(result.clone());
    assert!(!agent.busy);
    // a message of only pictures merged after another still counts as picked up
    agent.claude_waiting = VecDeque::from(["Look at these".to_owned(), String::new()]);
    agent.handle(echo("Look at these"));
    assert!(agent.claude_waiting.is_empty());
    agent.handle(result.clone());
    assert!(!agent.busy);
    // a background subagent waking claude up later runs a turn nobody sent, and still ends cleanly
    agent.handle(json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "it finished" }] } }));
    agent.handle(result);
    assert!(!agent.busy);
}

#[test]
fn claude_compaction_shows_in_the_transcript_and_shrinks_the_context() {
    let mut agent = claude_agent();
    agent.context = Some(ContextUsage {
        used: 150_000,
        window: 200_000,
    });
    agent.handle(json!({ "type": "system", "subtype": "status", "status": "compacting" }));
    assert_eq!(
        agent.items,
        [AgentItem::Compaction {
            status: ItemStatus::Running
        }]
    );
    agent.handle(json!({ "type": "system", "subtype": "compact_boundary",
        "compact_metadata": { "trigger": "auto", "pre_tokens": 150000, "post_tokens": 6000 } }));
    assert_eq!(
        agent.items,
        [AgentItem::Compaction {
            status: ItemStatus::Done
        }]
    );
    assert_eq!(agent.context.map(|c| c.used), Some(6000));
}

#[test]
fn codex_compaction_reuses_the_item_mux_started() {
    let mut agent = agent();
    agent.compacting = Some(agent.push(AgentItem::Compaction {
        status: ItemStatus::Running,
    }));
    let item = json!({ "item": { "id": "k1", "type": "contextCompaction" } });
    note(&mut agent, "item/started", item.clone());
    note(&mut agent, "item/completed", item);
    assert_eq!(
        agent.items,
        [AgentItem::Compaction {
            status: ItemStatus::Done
        }]
    );
    assert_eq!(agent.compacting, None);
}

fn subagent(
    agent: &Agent,
    index: usize,
) -> (
    ItemStatus,
    String,
    Vec<mux_core::protocol::SubagentStep>,
    String,
) {
    match &agent.items[index] {
        AgentItem::Subagent {
            status,
            activity,
            steps,
            result,
            ..
        } => (*status, activity.clone(), steps.clone(), result.clone()),
        other => panic!("not a subagent: {other:?}"),
    }
}

// shapes taken from what claude 2.1.312 streamed for two subagents, one in the background
#[test]
fn claude_subagents_get_cards_with_their_own_steps() {
    let mut agent = claude_agent();
    agent.handle(json!({ "type": "assistant", "message": { "content": [
        { "type": "tool_use", "id": "t1", "name": "Agent", "input": {
            "description": "List files", "prompt": "Use Glob", "subagent_type": "general-purpose" } },
        { "type": "tool_use", "id": "t2", "name": "Agent", "input": {
            "description": "Read a file", "prompt": "Use Read", "subagent_type": "general-purpose",
            "run_in_background": true } }
    ] } }));
    agent.handle(json!({ "type": "system", "subtype": "task_started", "task_id": "k2", "tool_use_id": "t2" }));
    agent.handle(
        json!({ "type": "assistant", "parent_tool_use_id": "t2", "message": { "content": [
        { "type": "tool_use", "id": "s1", "name": "Read", "input": { "file_path": "/tmp/a.jsonl" } }
    ] } }),
    );
    let (status, activity, steps, _) = subagent(&agent, 1);
    assert_eq!(status, ItemStatus::Running);
    assert_eq!(activity, "Read /tmp/a.jsonl");
    assert_eq!(steps[0].status, ItemStatus::Running);
    agent.handle(
        json!({ "type": "user", "parent_tool_use_id": "t2", "message": { "content": [
        { "type": "tool_result", "tool_use_id": "s1", "content": "line" }
    ] } }),
    );
    // the background one's tool result only says it launched, so it keeps running
    agent.handle(json!({ "type": "user", "message": { "content": [
        { "type": "tool_result", "tool_use_id": "t2", "content": "Async agent launched successfully." }
    ] } }));
    assert_eq!(subagent(&agent, 1).0, ItemStatus::Running);
    agent.handle(json!({ "type": "user", "tool_use_result": { "content": [{ "type": "text", "text": "a.jsonl" }] },
        "message": { "content": [{ "type": "tool_result", "tool_use_id": "t1", "content": "[hand-back] a.jsonl" }] } }));
    let (status, _, _, result) = subagent(&agent, 0);
    assert_eq!((status, result.as_str()), (ItemStatus::Done, "a.jsonl"));
    agent.handle(
        json!({ "type": "assistant", "parent_tool_use_id": "t2", "message": { "content": [
        { "type": "text", "text": "The first line is a system init." }
    ] } }),
    );
    agent.handle(json!({ "type": "system", "subtype": "task_notification", "task_id": "k2", "tool_use_id": "t2", "status": "completed" }));
    let (status, activity, steps, result) = subagent(&agent, 1);
    assert_eq!(status, ItemStatus::Done);
    assert!(activity.is_empty());
    assert_eq!(steps[0].status, ItemStatus::Done);
    assert_eq!(result, "The first line is a system init.");
}

#[test]
fn codex_subagent_threads_fill_their_card_and_leave_the_turn_alone() {
    let mut agent = agent();
    agent.thread_id = Some("main".into());
    agent.set_busy(true);
    note(
        &mut agent,
        "item/started",
        json!({ "threadId": "main", "item": {
        "id": "c1", "type": "collabAgentToolCall", "tool": "spawnAgent", "prompt": "Find the flaky test",
        "receiverThreadIds": ["child"], "agentsStates": {}, "status": "inProgress" } }),
    );
    note(
        &mut agent,
        "turn/completed",
        json!({ "threadId": "child", "turn": { "status": "completed" } }),
    );
    assert!(
        agent.busy,
        "a subagent's turn ending is not the main turn ending"
    );
    note(
        &mut agent,
        "item/started",
        json!({ "threadId": "child", "item": {
        "id": "x1", "type": "commandExecution", "command": "cargo test" } }),
    );
    note(
        &mut agent,
        "item/completed",
        json!({ "threadId": "child", "item": {
        "id": "x1", "type": "commandExecution", "status": "completed" } }),
    );
    note(
        &mut agent,
        "item/completed",
        json!({ "threadId": "main", "item": {
        "id": "c2", "type": "collabAgentToolCall", "tool": "wait",
        "agentsStates": { "child": { "status": "completed", "message": "It's session_expiry." } } } }),
    );
    let (status, _, steps, result) = subagent(&agent, 0);
    assert_eq!(status, ItemStatus::Done);
    assert_eq!(steps[0].name, "Bash");
    assert_eq!(steps[0].status, ItemStatus::Done);
    assert_eq!(result, "It's session_expiry.");
}

#[test]
fn sums_up_the_conversation_for_the_sidebar() {
    let mut agent = agent();
    agent.settings.model = Some("gpt-6.1-sol".into());
    agent.items = vec![
        AgentItem::User {
            text: "fix it".into(),
            attachments: Vec::new(),
            queued: false,
            rewind: None,
            took: None,
        },
        AgentItem::FileChange {
            changes: vec![FileChange {
                path: "a.rs".into(),
                diff: "--- a/a.rs\n+++ b/a.rs\n@@ -1 +1,2 @@\n-old\n+new\n+more".into(),
            }],
            status: ItemStatus::Done,
        },
        AgentItem::Assistant {
            text: "Fixed the flaky test.".into(),
        },
    ];
    let summary = agent.summary();
    assert_eq!(summary.model, "gpt-6.1-sol");
    assert_eq!(summary.preview, "Fixed the flaky test.");
    assert_eq!(
        (
            summary.messages,
            summary.files,
            summary.added,
            summary.removed
        ),
        (1, 1, 2, 1)
    );
}

// shapes from codex's request_user_input schema and claude's AskUserQuestion tool
#[test]
fn questions_wait_for_an_answer_from_either_agent() {
    let mut agent = agent();
    agent.handle(json!({ "id": 4, "method": "item/tool/requestUserInput", "params": {
        "itemId": "i", "threadId": "t", "turnId": "u", "isBlocking": true,
        "questions": [{ "id": "q1", "header": "Approach", "question": "Which way?",
            "options": [{ "label": "Fast", "description": "Skip tests" }, { "label": "Safe", "description": "Run tests" }] }]
    } }));
    assert_eq!(agent.status(), SessionStatus::Waiting);
    let AgentItem::Question {
        id,
        questions,
        answers: None,
        ..
    } = agent.items[0].clone()
    else {
        panic!("expected an open question");
    };
    assert_eq!(questions[0].options[1].label, "Safe");
    agent.answer(id, vec![vec!["Safe".into()]]);
    assert!(
        matches!(&agent.items[0], AgentItem::Question { answers: Some(a), .. } if a[0] == ["Safe"])
    );
    assert_ne!(agent.status(), SessionStatus::Waiting);

    let mut claude = claude_agent();
    claude.handle(json!({ "type": "control_request", "request_id": "r1", "request": {
        "subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": { "questions": [
            { "question": "Which files?", "header": "Scope", "multiSelect": true,
              "options": [{ "label": "src", "description": "" }, { "label": "tests", "description": "" }] }
        ] } } }));
    assert!(
        matches!(&claude.items[0], AgentItem::Question { questions, .. } if questions[0].multiple)
    );
    // a turn that ends without an answer leaves the question marked unanswered
    claude.handle(json!({ "type": "result", "subtype": "success", "result": "ok" }));
    assert!(
        matches!(&claude.items[0], AgentItem::Question { answers: Some(a), .. } if a.is_empty())
    );
}

#[test]
fn plans_are_caught_and_checklists_update_in_place() {
    let mut claude = claude_agent();
    claude.handle(json!({ "type": "control_request", "request_id": "r2", "request": {
        "subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": { "plan": "1. Read\n2. Fix" } } }));
    assert_eq!(
        claude.items,
        [AgentItem::Plan {
            text: "1. Read\n2. Fix".into()
        }]
    );
    assert!(
        claude.approvals.is_empty(),
        "a plan isn't a permission prompt"
    );

    let todos = |status: &str| {
        json!({ "type": "assistant", "message": { "content": [{
        "type": "tool_use", "id": format!("t-{status}"), "name": "TodoWrite", "input": { "todos": [
            { "content": "Read", "status": "completed", "activeForm": "Reading" },
            { "content": "Fix", "status": status, "activeForm": "Fixing" }
        ] } }] } })
    };
    claude.handle(todos("in_progress"));
    claude.handle(todos("completed"));
    let lists: Vec<_> = claude
        .items
        .iter()
        .filter(|i| matches!(i, AgentItem::Todos { .. }))
        .collect();
    assert_eq!(lists.len(), 1, "one checklist per turn");
    assert!(matches!(lists[0], AgentItem::Todos { steps } if steps[1].status == TodoStatus::Done));

    let mut codex = agent();
    note(
        &mut codex,
        "turn/plan/updated",
        json!({ "threadId": "t", "turnId": "u", "plan": [
        { "step": "Read", "status": "inProgress" }, { "step": "Fix", "status": "pending" }
    ] }),
    );
    assert!(
        matches!(&codex.items[0], AgentItem::Todos { steps } if steps[0].status == TodoStatus::Active)
    );
}

#[test]
fn messages_sent_mid_turn_wait_with_mux_and_go_one_per_turn() {
    let mut agent = claude_agent();
    let (tx, _rx) = mpsc::unbounded_channel();
    agent.process = Some(Process::fake(tx));
    agent.set_busy(true);
    let queue = |agent: &mut Agent, text: &str| {
        agent.queue.push(Queued {
            text: text.into(),
            attachments: Vec::new(),
        })
    };
    queue(&mut agent, "first");
    queue(&mut agent, "second");
    assert_eq!(
        agent.change_queue(1, QueueAction::Edit("second, edited".into())),
        None
    );
    let result = json!({ "type": "result", "subtype": "success", "result": "ok" });
    agent.handle(result.clone());
    // the turn ending sends just the next one
    assert!(matches!(agent.items.last(), Some(AgentItem::User { text, .. }) if text == "first"));
    assert_eq!(agent.queue.len(), 1);
    // a stop holds what's left
    agent.stop();
    assert!(agent.held);
    agent.claude_waiting.clear();
    agent.handle(result.clone());
    assert_eq!(agent.queue.len(), 1, "held messages don't go out");
    agent.resume();
    assert!(
        matches!(agent.items.last(), Some(AgentItem::User { text, .. }) if text == "second, edited")
    );
    assert!(agent.queue.is_empty());
    // removing drops it without sending
    queue(&mut agent, "never");
    agent.change_queue(0, QueueAction::Remove);
    assert!(agent.queue.is_empty());
}

// claude 2.1.312 read a message written during a running command right after that command's result
#[test]
fn waiting_messages_go_to_claude_at_its_next_tool_call() {
    let mut agent = claude_agent();
    let (tx, mut rx) = mpsc::unbounded_channel();
    agent.process = Some(Process::fake(tx));
    agent.set_busy(true);
    agent.queue.push(Queued {
        text: "also check the tests".into(),
        attachments: Vec::new(),
    });
    let tool = json!({ "type": "assistant", "message": { "content": [
        { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "sleep 8" } }
    ] } });
    agent.handle(tool.clone());
    assert!(agent.queue.is_empty());
    assert!(
        matches!(agent.items.last(), Some(AgentItem::User { text, .. }) if text == "also check the tests")
    );
    let sent: Vec<Value> = std::iter::from_fn(|| rx.try_recv().ok())
        .map(|line| serde_json::from_str(&line).unwrap())
        .collect();
    assert!(sent.iter().any(|m| m["type"] == "user"));

    // a queue held by a stop stays put
    agent.queue.push(Queued {
        text: "later".into(),
        attachments: Vec::new(),
    });
    agent.held = true;
    agent.handle(tool);
    assert_eq!(agent.queue.len(), 1);
}

#[test]
fn rewinding_cuts_the_chat_and_points_each_agent_back() {
    let user = |rewind: Option<&str>| AgentItem::User {
        text: "go".into(),
        attachments: Vec::new(),
        queued: false,
        rewind: rewind.map(str::to_owned),
        took: None,
    };
    let reply = AgentItem::Assistant {
        text: "done".into(),
    };

    let mut claude = claude_agent();
    claude.session_id = Some("s1".into());
    claude.items = vec![
        user(Some("")),
        reply.clone(),
        user(Some("r1")),
        reply.clone(),
    ];
    assert!(claude.rewind(1));
    assert_eq!(claude.items, [user(Some("")), reply.clone()]);
    assert_eq!(claude.resume_at.as_deref(), Some("r1"));
    let args = claude_args(&claude.settings, Some("s1"), Some("r1")).join(" ");
    assert!(args.contains("--resume s1 --resume-session-at r1"));
    // back to before the first message starts claude afresh
    assert!(claude.rewind(0));
    assert!(claude.items.is_empty());
    assert_eq!(claude.session_id, None);

    let mut codex = agent();
    codex.items = vec![user(None), reply.clone()];
    note(
        &mut codex,
        "turn/started",
        json!({ "turn": { "id": "turn-1" } }),
    );
    assert_eq!(codex.items[0], user(Some("turn-1")));
    codex.set_busy(false);
    codex.thread_id = Some("th".into());
    assert!(codex.rewind(0));
    assert_eq!(codex.pending_revert.as_deref(), Some("turn-1"));

    codex.set_busy(true);
    assert!(!codex.rewind(0), "nothing is rewound while the agent works");
}

#[test]
fn claude_messages_sent_mid_turn_wait_their_turn() {
    let mut agent = claude_agent();
    let user = |queued| AgentItem::User {
        text: String::new(),
        attachments: Vec::new(),
        queued,
        rewind: None,
        took: None,
    };
    agent.items = vec![user(false), user(true), user(true)];
    agent.claude_waiting = VecDeque::from(["a".to_owned(), "b".to_owned()]);
    agent.set_busy(true);
    let echo = |text: &str| json!({ "type": "user", "isReplay": true, "message": { "role": "user", "content": text } });
    let result = json!({ "type": "result", "subtype": "success", "result": "ok" });
    agent.handle(result.clone());
    agent.handle(echo("a"));
    assert_eq!(agent.items, [user(false), user(false), user(true)]);
    agent.handle(result.clone());
    agent.handle(echo("b"));
    agent.handle(result);
    assert_eq!(agent.items, [user(false), user(false), user(false)]);
    assert!(!agent.busy);
}

#[test]
fn a_steer_that_misses_the_turn_starts_its_own() {
    let mut agent = agent();
    agent.thread_id = Some("th".into());
    agent
        .calls
        .insert(4, Call::Steer("also this".into(), Vec::new()));
    agent.handle(json!({ "id": 4, "error": { "message": "no active turn" } }));
    assert!(
        agent.calls.values().any(|c| matches!(c, Call::StartTurn)),
        "the message goes out as a new turn"
    );
}

// the message order below is what codex 0.164 sent for a turn that ran one approved command
#[test]
fn maps_a_turn_with_an_approved_command() {
    let mut agent = agent();
    note(
        &mut agent,
        "turn/started",
        json!({ "turn": { "id": "t1" } }),
    );
    note(
        &mut agent,
        "item/started",
        json!({ "item": { "type": "userMessage", "id": "u1" } }),
    );
    note(
        &mut agent,
        "item/started",
        json!({ "item": { "type": "agentMessage", "id": "m1", "text": "" } }),
    );
    note(
        &mut agent,
        "item/agentMessage/delta",
        json!({ "itemId": "m1", "delta": "I'll" }),
    );
    note(
        &mut agent,
        "item/agentMessage/delta",
        json!({ "itemId": "m1", "delta": " create it" }),
    );
    assert_eq!(
        agent.items[0],
        AgentItem::Assistant {
            text: "I'll create it".into()
        }
    );
    note(
        &mut agent,
        "item/completed",
        json!({ "item": { "type": "agentMessage", "id": "m1", "text": "I'll create it.\n" } }),
    );

    note(
        &mut agent,
        "item/started",
        json!({ "item": {
        "type": "commandExecution", "id": "c1", "command": "bash -lc \"printf hi > hello.txt\"", "status": "inProgress"
    } }),
    );
    agent.handle(json!({ "id": 0, "method": "item/commandExecution/requestApproval", "params": { "itemId": "c1", "kind": "command" } }));
    assert_eq!(agent.status(), SessionStatus::Waiting);
    let AgentItem::Approval { id, detail, .. } = &agent.items[2] else {
        panic!("expected an approval, got {:?}", agent.items[2]);
    };
    assert_eq!(detail, "bash -lc \"printf hi > hello.txt\"");
    agent.decide(*id, Decision::Allow);
    assert_eq!(agent.status(), SessionStatus::Running);

    note(
        &mut agent,
        "item/commandExecution/outputDelta",
        json!({ "itemId": "c1", "delta": "partial" }),
    );
    note(
        &mut agent,
        "item/completed",
        json!({ "item": {
        "type": "commandExecution", "id": "c1", "status": "completed", "exitCode": 0, "aggregatedOutput": "hi\n"
    } }),
    );
    note(
        &mut agent,
        "turn/completed",
        json!({ "turn": { "id": "t1", "status": "completed" } }),
    );

    assert_eq!(
        agent.items,
        vec![
            AgentItem::Assistant {
                text: "I'll create it.\n".into()
            },
            AgentItem::Command {
                command: "bash -lc \"printf hi > hello.txt\"".into(),
                output: "hi\n".into(),
                exit_code: Some(0),
                status: ItemStatus::Done,
                background: false,
            },
            AgentItem::Approval {
                id: 1,
                title: "Run a command?".into(),
                detail: "bash -lc \"printf hi > hello.txt\"".into(),
                decision: Some(Decision::Allow),
            },
        ]
    );
    assert!(!agent.busy);
    assert_eq!(agent.status(), SessionStatus::Done);
}

#[test]
fn nonzero_exit_fails_and_unknown_requests_are_refused() {
    let mut agent = agent();
    note(
        &mut agent,
        "item/started",
        json!({ "item": { "type": "commandExecution", "id": "c1", "command": "false" } }),
    );
    note(
        &mut agent,
        "item/completed",
        json!({ "item": { "type": "commandExecution", "id": "c1", "status": "completed", "exitCode": 1 } }),
    );
    assert!(matches!(
        agent.items[0],
        AgentItem::Command {
            status: ItemStatus::Failed,
            exit_code: Some(1),
            ..
        }
    ));

    agent.handle(json!({ "id": 7, "method": "item/tool/call", "params": {} }));
    assert_eq!(
        agent.items.len(),
        1,
        "unsupported requests must not show up as approvals"
    );
}

#[test]
fn turns_carry_the_chosen_model_effort_and_speed() {
    let defaults = turn_params("t", "hi", &[], &AgentSettings::default());
    assert!(defaults.get("model").is_none() && defaults.get("effort").is_none());
    assert_eq!(defaults["serviceTierForTurn"], "default");

    let chosen = AgentSettings {
        model: Some("gpt-5.6-luna".into()),
        effort: Some("high".into()),
        fast: true,
        ..Default::default()
    };
    let params = turn_params("t", "hi", &[], &chosen);
    assert_eq!(params["model"], "gpt-5.6-luna");
    assert_eq!(params["effort"], "high");
    assert_eq!(params["serviceTierForTurn"], "priority");
    assert_eq!(params["input"][0]["text"], "hi");
}

#[test]
fn attachments_go_to_codex_as_images_and_paths() {
    let folder = std::env::temp_dir();
    let files = [
        "/p/shot.PNG".to_owned(),
        "/p/notes.md".to_owned(),
        folder.to_string_lossy().into_owned(),
    ];
    let input = &turn_params("t", "look", &files, &AgentSettings::default())["input"];
    assert_eq!(
        input[0]["text"],
        format!(
            "look\n\nAttached:\n- /p/notes.md\n- {} (folder)",
            folder.display()
        )
    );
    assert_eq!(
        input[1],
        json!({ "type": "localImage", "path": "/p/shot.PNG" })
    );
    assert_eq!(input.as_array().unwrap().len(), 2);
    // a folder sent on its own still reaches codex, with nothing blank ahead of it
    let alone = &turn_params("t", "", &files[1..2], &AgentSettings::default())["input"];
    assert_eq!(alone[0]["text"], "Attached:\n- /p/notes.md");
}

#[test]
fn attachments_go_to_claude_inline_or_as_paths() {
    let dir = std::env::temp_dir().join(format!("mux-attach-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let image = dir.join("dot.png");
    fs::write(&image, b"png").unwrap();
    let image = image.to_string_lossy().into_owned();
    let content = claude_content("look", &[image, "/p/notes.md".to_owned()], &[]);
    assert_eq!(content[0]["source"]["media_type"], "image/png");
    assert_eq!(content[0]["source"]["data"], "cG5n");
    assert_eq!(content[1]["text"], "look\n\nAttached:\n- /p/notes.md");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn turns_carry_the_access_mode() {
    let defaults = turn_params("t", "hi", &[], &AgentSettings::default());
    assert_eq!(defaults["approvalPolicy"], "on-request");
    assert_eq!(defaults["sandboxPolicy"]["type"], "workspaceWrite");

    let full = AgentSettings {
        access: Access::Full,
        ..Default::default()
    };
    let params = turn_params("t", "hi", &[], &full);
    assert_eq!(params["approvalPolicy"], "never");
    assert_eq!(params["sandboxPolicy"]["type"], "dangerFullAccess");

    let supervised = AgentSettings {
        access: Access::Supervised,
        ..Default::default()
    };
    assert_eq!(
        turn_params("t", "hi", &[], &supervised)["approvalPolicy"],
        "untrusted"
    );
}

fn written(rx: &mut mpsc::UnboundedReceiver<String>) -> Value {
    serde_json::from_str(&rx.try_recv().unwrap()).unwrap()
}

#[test]
fn codex_asks_before_mcp_tools_and_always_allowing_lasts_the_session() {
    let mut agent = agent();
    let (tx, mut rx) = mpsc::unbounded_channel();
    agent.process = Some(Process::fake(tx));
    agent.handle(json!({ "id": 3, "method": "mcpServer/elicitation/request", "params": {
        "serverName": "probe", "mode": "form", "message": "Allow?",
        "requestedSchema": { "type": "object", "properties": {} },
        "_meta": { "codex_approval_kind": "mcp_tool_call", "persist": ["session", "always"],
            "tool_name": "echo", "tool_params_display": [{ "name": "text", "display_name": "text", "value": "hi" }] }
    } }));
    let AgentItem::Approval {
        id, title, detail, ..
    } = agent.items[0].clone()
    else {
        panic!("expected an approval");
    };
    assert_eq!(
        (title.as_str(), detail.as_str()),
        ("Allow probe to use echo?", "text: hi")
    );
    agent.decide(id, Decision::AlwaysAllow);
    assert_eq!(
        written(&mut rx),
        json!({ "id": 3, "result": { "action": "accept", "_meta": { "persist": "session" } } })
    );
}

#[test]
fn a_sign_in_page_comes_with_a_link_to_open_but_only_a_web_one() {
    let mut agent = claude_agent();
    let ask = |url: &str| {
        json!({ "type": "control_request", "request_id": "r", "request": {
        "subtype": "elicitation", "mcp_server_name": "linear", "message": "Sign in", "mode": "url", "url": url } })
    };
    agent.handle(ask("https://linear.app/oauth"));
    agent.handle(ask("file:///etc/passwd"));
    let urls: Vec<Option<String>> = agent
        .items
        .iter()
        .map(|i| match i {
            AgentItem::Question {
                form: Some(form), ..
            } => form.url.clone(),
            _ => panic!("expected a form"),
        })
        .collect();
    assert_eq!(urls, [Some("https://linear.app/oauth".into()), None]);
}

#[test]
fn a_mux_server_that_fails_to_start_says_so_once() {
    let mut agent = agent();
    agent.started_mcp = vec![McpServer {
        name: "probe".into(),
        enabled: true,
        transport: mux_core::protocol::McpTransport::Http {
            url: "http://x".into(),
            headers: Vec::new(),
        },
    }];
    let status = |name: &str, status: &str| {
        json!({ "method": "mcpServer/startupStatus/updated",
        "params": { "name": name, "status": status, "error": "No such file or directory" } })
    };
    // codex retries a failed server, which is still only said once
    for _ in 0..2 {
        agent.handle(status("probe", "failed"));
    }
    agent.handle(status("their_own", "failed"));
    assert_eq!(agent.items.len(), 1);
    assert!(
        matches!(&agent.items[0], AgentItem::Error { message } if message.contains("probe") && message.contains("No such file"))
    );
    let states = agent.take_mcp_states();
    assert_eq!(states.len(), 2, "only mux's own servers are tracked");

    let mut claude = claude_agent();
    claude.started_mcp = agent.started_mcp.clone();
    claude.handle(
        json!({ "type": "system", "subtype": "init", "session_id": "s",
        "mcp_servers": [{ "name": "probe", "status": "connected" }] }),
    );
    assert_eq!(
        claude.take_mcp_states(),
        [("probe".to_owned(), McpState::Connected)]
    );
    assert!(claude.items.is_empty());
}

#[test]
fn mcp_forms_are_asked_and_answered_or_declined() {
    let schema = json!({ "type": "object", "required": ["color"], "properties": {
        "color": { "type": "string", "title": "Colour", "enum": ["red", "green"] } } });
    let mut codex = agent();
    let (tx, mut rx) = mpsc::unbounded_channel();
    codex.process = Some(Process::fake(tx));
    codex.handle(json!({ "id": 4, "method": "mcpServer/elicitation/request", "params": {
        "serverName": "probe", "mode": "form", "message": "Pick one", "_meta": null, "requestedSchema": schema } }));
    let AgentItem::Question {
        id,
        form: Some(form),
        questions,
        ..
    } = codex.items[0].clone()
    else {
        panic!("expected a form");
    };
    assert_eq!(
        (form.server.as_str(), form.message.as_str()),
        ("probe", "Pick one")
    );
    assert_eq!(questions[0].options.len(), 2);
    codex.answer(id, vec![vec!["green".into()]]);
    assert_eq!(
        written(&mut rx),
        json!({ "id": 4, "result": { "action": "accept", "content": { "color": "green" } } })
    );

    let mut claude = claude_agent();
    let (tx, mut rx) = mpsc::unbounded_channel();
    claude.process = Some(Process::fake(tx));
    claude.handle(
        json!({ "type": "control_request", "request_id": "r1", "request": {
        "subtype": "elicitation", "mcp_server_name": "probe", "message": "Pick one", "mode": "form",
        "requested_schema": schema, "property_order": ["color"] } }),
    );
    let AgentItem::Question { id, .. } = claude.items[0].clone() else {
        panic!("expected a form");
    };
    claude.answer(id, Vec::new());
    let sent = written(&mut rx);
    assert_eq!(sent["response"]["request_id"], "r1");
    assert_eq!(sent["response"]["response"], json!({ "action": "decline" }));
}

#[test]
fn an_idle_agent_starts_again_when_its_mcp_servers_change() {
    let mut agent = agent();
    let (tx, _rx) = mpsc::unbounded_channel();
    agent.process = Some(Process::fake(tx));
    agent.mcp = vec![McpServer {
        name: "probe".into(),
        enabled: true,
        transport: mux_core::protocol::McpTransport::Http {
            url: "http://x".into(),
            headers: Vec::new(),
        },
    }];
    let (events, _) = mpsc::unbounded_channel();
    let cwd = PathBuf::from("/nonexistent/mux");
    agent.started_mcp = agent.mcp.clone();
    agent.send("hi".into(), Vec::new(), 1, 1, cwd.clone(), &events);
    assert!(
        agent.process.is_some(),
        "an unchanged list keeps the running cli"
    );
    agent.busy = false;
    agent.mcp.clear();
    // a helper still working in the background keeps the old cli until it's done
    agent.items.push(AgentItem::Subagent {
        task: "t".into(),
        kind: String::new(),
        prompt: String::new(),
        background: true,
        status: ItemStatus::Running,
        activity: String::new(),
        steps: Vec::new(),
        result: String::new(),
        tokens: 0,
    });
    agent.send("hi".into(), Vec::new(), 1, 1, cwd.clone(), &events);
    assert!(
        agent.process.is_some(),
        "the background helper is left alone"
    );
    agent.busy = false;
    agent.items.clear();
    // the folder doesn't exist so the new start fails, which still shows the old process was let go
    agent.send("hi".into(), Vec::new(), 1, 1, cwd, &events);
    assert!(agent.process.is_none());
}

fn claude_agent() -> Agent {
    let mut agent = agent();
    agent.settings.provider = Provider::Claude;
    agent
}

#[test]
fn tracks_how_full_the_context_window_is() {
    let mut agent = agent();
    agent.handle(json!({ "method": "thread/tokenUsage/updated", "params": { "tokenUsage": {
        "last": { "totalTokens": 24000 }, "total": { "totalTokens": 90000 }, "modelContextWindow": 272000
    } } }));
    assert_eq!(
        agent.take_context(),
        Some(ContextUsage {
            used: 24000,
            window: 272000
        })
    );
    assert_eq!(agent.take_context(), None);

    // the numbers are what claude 2.1.312 reported for a one word haiku reply
    let mut claude = claude_agent();
    claude.handle(json!({ "type": "assistant", "message": { "content": [], "usage": {
        "input_tokens": 2, "cache_creation_input_tokens": 12223, "cache_read_input_tokens": 11383, "output_tokens": 4
    } } }));
    claude.handle(
        json!({ "type": "result", "subtype": "success", "result": "ok",
        "modelUsage": { "claude-haiku-5-5": { "contextWindow": 1000000 } } }),
    );
    assert_eq!(
        claude.take_context(),
        Some(ContextUsage {
            used: 23612,
            window: 1000000
        })
    );
}

// the frame order below is what claude 2.1.312 sent for a turn whose bash call was denied
#[test]
fn maps_a_claude_turn_with_a_denied_command() {
    let mut agent = claude_agent();
    agent.handle(json!({ "type": "system", "subtype": "init", "session_id": "s1" }));
    agent.handle(json!({ "type": "assistant", "message": { "content": [
        { "type": "tool_use", "id": "t1", "name": "Bash", "input": { "command": "touch denied.txt" } }
    ] } }));
    agent.handle(
        json!({ "type": "control_request", "request_id": "r1", "request": {
        "subtype": "can_use_tool", "tool_name": "Bash", "input": { "command": "touch denied.txt" },
        "permission_suggestions": [{ "type": "addRules", "destination": "localSettings" }]
    } }),
    );
    assert_eq!(agent.status(), SessionStatus::Waiting);
    let AgentItem::Approval { id, detail, .. } = agent.items[1].clone() else {
        panic!("expected an approval, got {:?}", agent.items[1]);
    };
    assert_eq!(detail, "touch denied.txt");
    agent.decide(id, Decision::Deny);

    agent.handle(json!({ "type": "user", "message": { "content": [
        { "type": "tool_result", "tool_use_id": "t1", "content": "The user declined this in Mux.", "is_error": true }
    ] } }));
    agent.handle(json!({ "type": "stream_event", "event": { "type": "content_block_start", "content_block": { "type": "text" } } }));
    agent.handle(json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "Declined" } } }));
    assert_eq!(
        agent.items[2],
        AgentItem::Assistant {
            text: "Declined".into()
        }
    );
    agent.handle(json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "Declined, so I left it." }] } }));
    agent.handle(json!({ "type": "result", "subtype": "success", "is_error": false }));

    assert_eq!(agent.session_id.as_deref(), Some("s1"));
    assert!(
        matches!(&agent.items[0], AgentItem::Command { status: ItemStatus::Failed, output, .. } if output.contains("declined"))
    );
    assert!(matches!(
        agent.items[1],
        AgentItem::Approval {
            decision: Some(Decision::Deny),
            ..
        }
    ));
    assert_eq!(
        agent.items[2],
        AgentItem::Assistant {
            text: "Declined, so I left it.".into()
        }
    );
    assert_eq!(
        agent.items.len(),
        3,
        "the streamed reply and its final copy are one item"
    );
    assert!(!agent.busy);
}

#[test]
fn claude_edits_become_diffs_and_always_allow_stays_in_the_session() {
    let mut agent = claude_agent();
    agent.handle(json!({ "type": "assistant", "message": { "content": [
        { "type": "tool_use", "id": "e1", "name": "Edit", "input": { "file_path": "src/a.rs", "old_string": "old", "new_string": "new" } },
        { "type": "tool_use", "id": "g1", "name": "Grep", "input": { "pattern": "SessionStore" } }
    ] } }));
    let AgentItem::FileChange { changes, .. } = &agent.items[0] else {
        panic!()
    };
    assert_eq!(changes[0].path, "src/a.rs");
    assert_eq!(changes[0].diff, "@@\n-old\n+new\n");
    assert!(
        matches!(&agent.items[1], AgentItem::Tool { name, detail, .. } if name == "Grep" && detail == "SessionStore")
    );

    let answer = claude_answer(
        Decision::AlwaysAllow,
        json!({ "command": "ls" }),
        &json!([{ "type": "addRules", "destination": "localSettings" }]),
    );
    assert_eq!(answer["updatedPermissions"][0]["destination"], "session");
    assert_eq!(answer["updatedInput"]["command"], "ls");
}

#[test]
fn claude_launches_with_the_chosen_settings_and_session() {
    let settings = AgentSettings {
        provider: Provider::Claude,
        model: Some("opus".into()),
        effort: Some("high".into()),
        fast: true,
        access: Access::AutoEdits,
        worktree: false,
        plan: false,
    };
    let args = claude_args(&settings, Some("s1"), None).join(" ");
    assert!(args.contains("--permission-prompt-tool stdio"));
    assert!(args.contains("--permission-mode acceptEdits"));
    assert!(args.contains("--model opus") && args.contains("--effort high"));
    assert!(args.contains(r#"--settings {"fastMode":true}"#));
    assert!(args.ends_with("--resume s1"));

    let full = AgentSettings {
        access: Access::Full,
        ..settings
    };
    let args = claude_args(&full, None, None).join(" ");
    assert!(
        args.contains("--permission-mode bypassPermissions --allow-dangerously-skip-permissions")
    );
}

#[test]
fn a_dropped_turn_expires_its_approvals() {
    let mut agent = agent();
    agent.handle(json!({ "id": 0, "method": "item/fileChange/requestApproval", "params": { "itemId": "missing" } }));
    assert_eq!(agent.status(), SessionStatus::Waiting);
    note(
        &mut agent,
        "turn/completed",
        json!({ "turn": { "status": "interrupted" } }),
    );
    assert!(matches!(
        agent.items[0],
        AgentItem::Approval {
            decision: Some(Decision::Deny),
            ..
        }
    ));
    assert_ne!(agent.status(), SessionStatus::Waiting);
}

// run with: cargo test -p mux-server --lib --release bench_ -- --ignored --nocapture
#[test]
#[ignore]
fn bench_saving_a_long_chat() {
    let mut agent = agent();
    for turn in 0..200 {
        agent.items.push(AgentItem::User {
            text: format!("message {turn}"),
            attachments: Vec::new(),
            queued: false,
            rewind: None,
            took: None,
        });
        for _ in 0..10 {
            agent.items.push(AgentItem::Command {
                command: "cargo test --workspace".into(),
                output: "test result: ok\n".repeat(1600),
                exit_code: Some(0),
                status: ItemStatus::Done,
                background: false,
            });
        }
    }
    let path = std::env::temp_dir().join(format!("mux-bench-{}.json", std::process::id()));
    let start = std::time::Instant::now();
    agent.save(&path).unwrap();
    let saved = start.elapsed();
    let size = std::fs::metadata(&path).unwrap().len() / (1024 * 1024);
    let start = std::time::Instant::now();
    let loaded = Agent::load(&path);
    println!(
        "{size} MB: saving {saved:?}, loading {:?}, {} items",
        start.elapsed(),
        loaded.items.len()
    );
    std::fs::remove_file(&path).unwrap();
}
