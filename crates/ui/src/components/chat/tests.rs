use super::*;

fn user(text: &str) -> ChatItem {
    ChatItem::User {
        text: text.into(),
        attachments: Vec::new(),
        queued: false,
        took: None,
    }
}

fn tool(kind: ToolKind, status: ToolStatus) -> ChatItem {
    ChatItem::Tool(ToolCall {
        kind,
        target: "x".into(),
        status,
        output: None,
        background: false,
        images: Vec::new(),
    })
}

fn reply(text: &str) -> ChatItem {
    ChatItem::Assistant(text.into())
}

#[test]
fn a_usage_limit_notice_stays_visible_when_other_work_is_folded() {
    let items = vec![
        user("go"),
        tool(ToolKind::Read, ToolStatus::Done),
        ChatItem::UsageLimit {
            message: "Limit reached".into(),
            retry_at: Some(100),
        },
    ];
    for running in [false, true] {
        let turn = fold(&items, 1, running);
        assert_eq!(turn.kept, [2]);
        assert_eq!(turn.work, [1]);
    }
}

#[test]
fn a_finished_turn_keeps_only_its_answer_in_view() {
    let items = vec![
        user("fix it"),
        ChatItem::Reasoning {
            text: "hm".into(),
            started: 0,
            ended: Some(4000),
        },
        tool(ToolKind::Read, ToolStatus::Done),
        reply("Found it, editing now."),
        tool(ToolKind::Run, ToolStatus::Done),
        ChatItem::Error("the test runner crashed".into()),
        reply("Fixed, and the tests pass."),
        user("thanks"),
    ];
    let turn = fold(&items, 1, false);
    assert!(turn.worth_folding(&items));
    assert_eq!(turn.work, [1, 2, 3, 4]);
    assert_eq!(turn.kept, [5, 6], "the error and the answer");
    assert_eq!(turn.end, 7);
    let work: Vec<&ChatItem> = turn.work.iter().map(|&i| &items[i]).collect();
    assert_eq!(
        cards::turn_summary(&work, false, None),
        "Read 1 file · Ran 1 command"
    );
    assert_eq!(
        cards::turn_summary(&work, false, Some(192_000)),
        "Worked for 3m 12s · Read 1 file · Ran 1 command"
    );
    assert_eq!(
        cards::turn_summary(&work, false, Some(400)),
        "Read 1 file · Ran 1 command",
        "a turn over in a moment isn't worth timing"
    );
}

#[test]
fn a_running_turn_folds_nothing_away_but_what_waits_on_the_person_stays_out() {
    let mut items = vec![
        user("go"),
        tool(ToolKind::Run, ToolStatus::Done),
        reply("Now the next step."),
        ChatItem::Permission(PermissionRequest {
            title: "Run a command?".into(),
            detail: None,
            decision: None,
        }),
    ];
    let turn = fold(&items, 1, true);
    assert!(turn.live);
    assert_eq!(turn.work, [1, 2], "the reply so far may not be the answer");
    assert_eq!(turn.kept, [3], "the approval is never folded away");

    items.push(ChatItem::Thinking);
    let turn = fold(&items, 1, true);
    assert_eq!(turn.kept, [3, 4]);
}

#[test]
fn what_a_turn_leaves_behind_stays_in_view_once_its_done() {
    let items = vec![
        user("plan it"),
        tool(ToolKind::Search, ToolStatus::Done),
        ChatItem::Plan {
            text: "1. do it".into(),
            actionable: true,
        },
        ChatItem::Changes(Changes {
            files: 2,
            added: 3,
            removed: 1,
            first: 1,
        }),
    ];
    let turn = fold(&items, 1, false);
    assert_eq!(turn.work, [1]);
    assert_eq!(turn.kept, [2, 3]);
    // a question and answer with nothing in between has nothing to fold
    let chat = vec![user("hi"), reply("hello")];
    assert!(fold(&chat, 1, false).work.is_empty());
    // and one step already reads as a line of its own
    assert!(!turn.worth_folding(&items));
    let thought = ChatItem::Reasoning {
        text: String::new(),
        started: 0,
        ended: Some(1000),
    };
    let streaming = vec![user("hi"), thought, reply("Hel")];
    let live = fold(&streaming, 1, true);
    assert_eq!(live.work, [1, 2]);
    assert!(
        !live.worth_folding(&streaming),
        "a reply streaming in isn't a step"
    );
}

#[test]
fn a_turn_with_only_thinking_says_how_long_it_thought() {
    let thought = ChatItem::Reasoning {
        text: String::new(),
        started: 1000,
        ended: Some(13000),
    };
    assert_eq!(
        cards::turn_summary(&[&thought], false, None),
        "Thought for 12s"
    );
    assert_eq!(
        cards::turn_summary(&[&thought], true, Some(9000)),
        "Working",
        "a running turn has no length yet"
    );
}

// how many vertices a chat needs drawn, which has to fit the gpu's 256 MB buffer at 20 bytes each
fn vertices(
    items: &[ChatItem],
    queue: &[mux_core::protocol::Queued],
    background: &[mux_core::protocol::Background],
) -> usize {
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let input = || egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            pos2(0.0, 0.0),
            egui::vec2(1400.0, 900.0),
        )),
        ..Default::default()
    };
    let mut most = 0;
    // the first frames only measure, and animations and images settle over the ones after
    for _ in 0..3 {
        let mut output = ctx.run_ui(input(), |ui| {
            let mut draft = String::new();
            ChatView::new(items, &mut draft)
                .queue(queue, false)
                .background(background)
                .show(ui);
        });
        output.textures_delta.clear();
        let count = ctx
            .tessellate(output.shapes, output.pixels_per_point)
            .iter()
            .map(|primitive| match &primitive.primitive {
                egui::epaint::Primitive::Mesh(mesh) => mesh.vertices.len(),
                egui::epaint::Primitive::Callback(_) => 0,
            })
            .sum();
        most = usize::max(most, count);
    }
    most
}

// one huge unbroken line in every piece of text an agent controls, which once crashed the app
#[test]
fn no_text_from_an_agent_can_overflow_the_gpu() {
    use mux_core::protocol::{
        Background, BackgroundState, Question, QuestionOption, Queued, TodoStatus, TodoStep,
    };
    const LIMIT: usize = 1024 * 1024;
    let long = "x".repeat(1024 * 1024);
    let call = |target: &str, output: Option<&str>| ToolCall {
        kind: ToolKind::Run,
        target: target.to_owned(),
        status: ToolStatus::Done,
        output: output.map(str::to_owned),
        background: false,
        images: Vec::new(),
    };
    let helper = |task: &str, kind: &str, activity: &str, result: &str, status| {
        ChatItem::Subagent(Subagent {
            provider: mux_core::Provider::Claude,
            task: task.to_owned(),
            kind: kind.to_owned(),
            status,
            background: false,
            activity: activity.to_owned(),
            steps: Vec::new(),
            result: result.to_owned(),
            tokens: 0,
        })
    };
    let question =
        |header: &str, question: &str, label: &str, description: &str| ChatItem::Question {
            questions: vec![Question {
                id: "q".into(),
                header: header.to_owned(),
                question: question.to_owned(),
                options: vec![QuestionOption {
                    label: label.to_owned(),
                    description: description.to_owned(),
                }],
                multiple: false,
                other: false,
                optional: false,
            }],
            answers: None,
            form: None,
        };
    let permission = |title: &str, detail: &str, decision| {
        ChatItem::Permission(PermissionRequest {
            title: title.to_owned(),
            detail: Some(detail.to_owned()),
            decision,
        })
    };
    let cases: Vec<(&str, ChatItem)> = vec![
        ("user", user(&long)),
        (
            "attachment",
            ChatItem::User {
                text: "go".into(),
                attachments: vec![long.clone()],
                queued: false,
                took: None,
            },
        ),
        ("reply", reply(&long)),
        ("code", reply(&format!("```\n{long}\n```"))),
        ("inline code", reply(&format!("`{long}`"))),
        ("heading", reply(&format!("# {long}"))),
        ("list", reply(&format!("- {long}"))),
        ("quote", reply(&format!("> {long}"))),
        ("link", reply(&format!("[{long}](https://example.com)"))),
        ("table head", reply(&format!("| {long} |\n|---|\n| b |"))),
        ("table cell", reply(&format!("| a |\n|---|\n| {long} |"))),
        ("error", ChatItem::Error(long.clone())),
        (
            "limit",
            ChatItem::UsageLimit {
                message: long.clone(),
                retry_at: None,
            },
        ),
        ("tool", ChatItem::Tool(call(&long, None))),
        ("tool output", ChatItem::Tool(call("x", Some(&long)))),
        (
            "diff",
            ChatItem::Diff(FileDiff {
                path: "a".into(),
                lines: vec![DiffLine {
                    kind: DiffKind::Add,
                    text: long.clone(),
                }],
            }),
        ),
        (
            "diff path",
            ChatItem::Diff(FileDiff {
                path: long.clone(),
                lines: Vec::new(),
            }),
        ),
        ("asking title", permission(&long, "x", None)),
        ("asking detail", permission("x", &long, None)),
        (
            "decided title",
            permission(&long, "x", Some(Decision::Allow)),
        ),
        (
            "decided detail",
            permission("x", &long, Some(Decision::Allow)),
        ),
        ("helper task", helper(&long, "x", "", "", ToolStatus::Done)),
        ("helper kind", helper("x", &long, "", "", ToolStatus::Done)),
        (
            "helper activity",
            helper("x", "x", &long, "", ToolStatus::Running),
        ),
        (
            "helper result",
            helper("x", "x", "", &long, ToolStatus::Done),
        ),
        ("question header", question(&long, "x", "x", "x")),
        ("question", question("x", &long, "x", "x")),
        ("option", question("x", "x", &long, "x")),
        ("option text", question("x", "x", "x", &long)),
        (
            "plan",
            ChatItem::Plan {
                text: long.clone(),
                actionable: true,
            },
        ),
        (
            "todo",
            ChatItem::Todos(vec![TodoStep {
                text: long.clone(),
                status: TodoStatus::Pending,
            }]),
        ),
        (
            "reasoning",
            ChatItem::Reasoning {
                text: long.clone(),
                started: 0,
                ended: Some(1),
            },
        ),
        (
            "retry",
            ChatItem::Retry {
                reason: long.clone(),
                attempt: 1,
                of: None,
                status: ToolStatus::Running,
            },
        ),
    ];
    let mut over = Vec::new();
    for (name, item) in cases {
        let items = vec![user("go"), item];
        let count = vertices(&items, &[], &[]);
        if count > LIMIT {
            over.push(format!("{name}: {count}"));
        }
    }
    let queued = [Queued {
        text: long.clone(),
        attachments: Vec::new(),
    }];
    let count = vertices(&[user("go")], &queued, &[]);
    if count > LIMIT {
        over.push(format!("queued message: {count}"));
    }
    let task = |label: &str, command: &str| Background {
        id: "b".into(),
        label: label.to_owned(),
        command: command.to_owned(),
        started: 0,
        state: BackgroundState::Running,
        output: String::new(),
        stoppable: false,
    };
    for (name, task) in [
        ("task label", task(&long, "x")),
        ("task command", task("x", &long)),
    ] {
        let count = vertices(&[user("go")], &[], &[task]);
        if count > LIMIT {
            over.push(format!("{name}: {count}"));
        }
    }
    assert!(over.is_empty(), "too many vertices to draw: {over:#?}");
}

// run with: cargo test -p mux-ui --release bench_ -- --ignored --nocapture
// a chat with folded turns, loose steps and replies of very different heights
fn mixed_chat(turns: usize, salt: usize) -> Vec<ChatItem> {
    let mut items = Vec::new();
    for turn in 0..turns {
        items.push(user(&format!("message {salt} {turn}")));
        if turn % 3 != 2 {
            for step in 0..3 {
                items.push(ChatItem::Tool(ToolCall {
                    kind: ToolKind::Run,
                    target: format!("step {salt} {turn} {step}"),
                    status: ToolStatus::Done,
                    output: Some("line\n".repeat(20)),
                    background: false,
                    images: Vec::new(),
                }));
            }
        }
        items.push(reply(
            &format!("Reply {salt} {turn}. **Done** with `code`, and enough words after that to wrap differently once the window gets narrower than it was before.\n\n")
                .repeat(1 + (turn + salt) % 9),
        ));
    }
    items
}

struct Window {
    ctx: egui::Context,
    size: egui::Vec2,
    time: f64,
    session: u64,
}

impl Window {
    fn new() -> Self {
        let ctx = egui::Context::default();
        theme::install(&ctx);
        Self {
            ctx,
            size: egui::vec2(1400.0, 900.0),
            time: 0.0,
            session: 0,
        }
    }

    // the words on screen and where they are, and how many were drawn anywhere
    fn frame(
        &mut self,
        items: &[ChatItem],
        events: Vec<egui::Event>,
    ) -> (Vec<(i32, i32, String)>, usize) {
        self.time += 1.0 / 60.0;
        let screen = Rect::from_min_size(pos2(0.0, 0.0), self.size);
        let input = egui::RawInput {
            screen_rect: Some(screen),
            time: Some(self.time),
            predicted_dt: 1.0 / 60.0,
            events,
            ..Default::default()
        };
        let mut draft = String::new();
        let session = self.session;
        let mut output = self.ctx.run_ui(input, |ui| {
            ChatView::new(items, &mut draft).session(session).show(ui);
        });
        output.textures_delta.clear();
        let mut seen = Vec::new();
        let mut drawn = 0;
        for clipped in &output.shapes {
            let egui::Shape::Text(text) = &clipped.shape else {
                continue;
            };
            drawn += 1;
            let rect = text.visual_bounding_rect().intersect(clipped.clip_rect);
            if rect.is_positive() && rect.intersects(screen) {
                seen.push((
                    (text.pos.x * 4.0) as i32,
                    (text.pos.y * 4.0) as i32,
                    text.galley.job.text.clone(),
                ));
            }
        }
        seen.sort();
        (seen, drawn)
    }

    fn settle(&mut self, items: &[ChatItem]) -> (Vec<(i32, i32, String)>, usize) {
        for _ in 0..90 {
            self.frame(items, Vec::new());
        }
        self.frame(items, Vec::new())
    }

    fn scroll(&mut self, items: &[ChatItem], by: f32) -> (Vec<(i32, i32, String)>, usize) {
        let events = vec![
            egui::Event::PointerMoved(pos2(self.size.x / 2.0, 300.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, by),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: Default::default(),
            },
        ];
        self.frame(items, events);
        self.settle(items)
    }

    // what the same frame looks like with every part of the chat drawn
    fn drawn_in_full(&mut self, items: &[ChatItem]) -> (Vec<(i32, i32, String)>, usize) {
        self.ctx.data_mut(|d| d.remove_by_type::<Measured>());
        self.frame(items, Vec::new())
    }

    fn click(&mut self, items: &[ChatItem], at: egui::Pos2) {
        let button = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        self.frame(items, vec![egui::Event::PointerMoved(at)]);
        self.frame(items, vec![button(true)]);
        self.frame(items, vec![button(false)]);
    }
}

#[test]
fn skipping_what_is_off_screen_changes_nothing_on_screen() {
    let items = mixed_chat(120, 0);
    let mut window = Window::new();
    let (bottom, _) = window.settle(&items);
    let mut skipped = 0;
    let mut views = vec![bottom];
    // far enough to pass through the whole chat in both directions
    for by in [
        700.0, 2500.0, 6000.0, 400.0, 90000.0, -3000.0, -500.0, -90000.0, 2000.0,
    ] {
        let (lazily, drawn) = window.scroll(&items, by);
        let (fully, all) = window.drawn_in_full(&items);
        assert_eq!(lazily, fully, "after scrolling by {by}");
        assert!(!lazily.is_empty());
        if drawn * 2 < all {
            skipped += 1;
        }
        views.push(lazily);
    }
    assert!(skipped >= 5, "off-screen parts were drawn anyway");
    views.dedup();
    assert!(views.len() >= 6, "the scrolling never moved the chat");
}

#[test]
fn opening_a_step_or_resizing_keeps_the_chat_in_place() {
    let items = mixed_chat(120, 0);
    let mut window = Window::new();
    window.settle(&items);
    let (before, _) = window.scroll(&items, 6000.0);
    // a turn's fold row, the way a person would open one
    let label = cards::turn_summary(&[&items[1], &items[2], &items[3]], false, None);
    let (x, y, _) = before
        .iter()
        .find(|(_, _, text)| *text == label)
        .expect("a folded turn on screen")
        .clone();
    window.click(&items, pos2(x as f32 / 4.0 + 4.0, y as f32 / 4.0 + 8.0));
    let (opened, _) = window.settle(&items);
    assert_ne!(opened, before, "the click opened nothing");
    assert_eq!(opened, window.drawn_in_full(&items).0, "after opening");
    window.settle(&items);
    window.size = egui::vec2(420.0, 800.0);
    let (narrow, _) = window.settle(&items);
    assert_eq!(narrow, window.drawn_in_full(&items).0, "after narrowing");
    let (scrolled, _) = window.scroll(&items, 3000.0);
    assert_eq!(
        scrolled,
        window.drawn_in_full(&items).0,
        "scrolled when narrow"
    );
}

#[test]
fn a_reply_growing_off_screen_can_be_scrolled_to_in_one_go() {
    let mut items = mixed_chat(120, 0);
    let mut window = Window::new();
    window.settle(&items);
    window.scroll(&items, 9000.0);
    let last = items.len() - 1;
    for _ in 0..200 {
        if let ChatItem::Assistant(text) = &mut items[last] {
            text.push_str("\n\nmore text arriving");
        }
        window.frame(&items, Vec::new());
    }
    if let ChatItem::Assistant(text) = &mut items[last] {
        text.push_str("\n\nthe very end");
    }
    window.frame(&items, Vec::new());
    let (bottom, _) = window.scroll(&items, -900000.0);
    assert!(bottom.iter().any(|(_, _, text)| text == "the very end"));
    assert_eq!(bottom, window.drawn_in_full(&items).0);
}

#[test]
fn each_session_measures_its_own_chat() {
    let (first, second) = (mixed_chat(120, 0), mixed_chat(120, 4));
    let mut window = Window::new();
    window.settle(&first);
    window.session = 1;
    window.settle(&second);
    let (lazily, _) = window.scroll(&second, 6000.0);
    assert_eq!(lazily, window.drawn_in_full(&second).0);
    window.session = 0;
    let (back, _) = window.settle(&first);
    assert!(back.iter().any(|(_, _, text)| text.contains("Reply 0 ")));
    assert_eq!(back, window.drawn_in_full(&first).0);
}

#[test]
fn a_selection_survives_scrolling_while_it_stays_on_screen() {
    use egui::text_selection::LabelSelectionState;
    let items = mixed_chat(120, 0);
    let mut window = Window::new();
    window.settle(&items);
    let (view, _) = window.scroll(&items, 9000.0);
    // near the top, so scrolling up moves it down the screen and keeps it in view
    let (x, y, _) = view
        .iter()
        .filter(|(_, y, text)| text.starts_with("Reply") && *y > 100 * 4)
        .min_by_key(|(_, y, _)| *y)
        .expect("a reply on screen")
        .clone();
    let (from, to) = (
        pos2(x as f32 / 4.0 + 2.0, y as f32 / 4.0 + 8.0),
        pos2(x as f32 / 4.0 + 90.0, y as f32 / 4.0 + 8.0),
    );
    let button = |pressed, pos| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    };
    window.frame(&items, vec![egui::Event::PointerMoved(from)]);
    window.frame(&items, vec![button(true, from)]);
    window.frame(&items, vec![egui::Event::PointerMoved(to)]);
    window.frame(&items, vec![button(false, to)]);
    let selected = |window: &Window| {
        window
            .ctx
            .plugin::<LabelSelectionState>()
            .lock()
            .has_selection()
    };
    assert!(selected(&window), "the drag selected nothing");
    let mut last = view;
    for _ in 0..4 {
        let (moved, _) = window.scroll(&items, 100.0);
        assert_ne!(moved, last, "the chat didn't scroll");
        assert!(selected(&window), "scrolling lost the selection");
        last = moved;
    }
}

#[test]
#[ignore]
fn bench_drawing_a_long_chat() {
    let mut items = Vec::new();
    for turn in 0..200 {
        items.push(user(&format!("message {turn}")));
        for _ in 0..10 {
            items.push(ChatItem::Tool(ToolCall {
                kind: ToolKind::Run,
                target: "cargo test --workspace".into(),
                status: ToolStatus::Done,
                output: Some("test result: ok\n".repeat(1600)),
                background: false,
                images: Vec::new(),
            }));
        }
        items.push(ChatItem::Diff(FileDiff {
            path: "/tmp/src/lib.rs".into(),
            lines: (0..200)
                .map(|_| DiffLine {
                    kind: DiffKind::Add,
                    text: "let a = 1;".into(),
                })
                .collect(),
        }));
        items.push(reply(
            &"Fixed it. Here's **what** changed:\n\n- one\n- two\n\n".repeat(20),
        ));
    }
    let ctx = egui::Context::default();
    theme::install(&ctx);
    let input = || egui::RawInput {
        screen_rect: Some(Rect::from_min_size(
            pos2(0.0, 0.0),
            egui::vec2(1400.0, 900.0),
        )),
        ..Default::default()
    };
    let mut draft = String::new();
    let mut frame = |label: &str| {
        let start = std::time::Instant::now();
        let mut output = ctx.run_ui(input(), |ui| {
            ChatView::new(&items, &mut draft).show(ui);
        });
        let laid_out = start.elapsed();
        output.textures_delta.clear();
        let meshes = ctx.tessellate(output.shapes, output.pixels_per_point);
        println!(
            "{label}: {laid_out:?} to lay out, {:?} in all, {} meshes",
            start.elapsed(),
            meshes.len()
        );
    };
    frame("first frame");
    frame("second frame");
    for _ in 0..3 {
        frame("settled frame");
    }
    let mut window = Window::new();
    window.settle(&items);
    window.frame(&items, Vec::new());
    let mut worst = std::time::Duration::ZERO;
    for _ in 0..600 {
        let wheel = vec![
            egui::Event::PointerMoved(pos2(700.0, 300.0)),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: egui::vec2(0.0, 400.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Unknown,
                modifiers: Default::default(),
            },
        ];
        let start = std::time::Instant::now();
        window.frame(&items, wheel);
        worst = worst.max(start.elapsed());
    }
    println!("scrolling: worst frame {worst:?}");
}
