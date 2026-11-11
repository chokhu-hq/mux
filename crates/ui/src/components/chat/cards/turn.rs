use super::*;

// what the model thought, folded to how long it took, with the thoughts a click away when the cli shares them
pub fn reasoning(ui: &mut Ui, text: &str, started: u64, ended: Option<u64>) {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("reasoning");
    let text = text.trim();
    let readable = !text.is_empty();
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let sense = if readable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), sense);
    if response.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    let t = if readable {
        hover_t(ui, &response)
    } else {
        0.0
    };
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius::MD),
        p.hover.gamma_multiply(t),
    );
    let y = rect.center().y;
    let turn = spring_bool(ui.ctx(), id.with("caret"), open, Spring::SMOOTH);
    let mut x = rect.left() + space::SM;
    // only thoughts that can be read open, so only they get a caret
    if readable {
        caret(ui, pos2(x + 6.0, y), turn, p.text_faint);
    }
    x += 12.0 + space::SM;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let label = match ended {
        Some(ended) => format!("Thought for {}", duration(ended.saturating_sub(started))),
        None => {
            spinner(ui, pos2(x + icon::MD / 2.0, y), icon::MD, p.text_muted);
            x += icon::MD + space::SM;
            // the running time ticks over each second
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
            format!("Thinking for {}", duration(now.saturating_sub(started)))
        }
    };
    ui.painter().text(
        pos2(x, y),
        Align2::LEFT_CENTER,
        label,
        font::medium(text::MD),
        p.text_muted.lerp_to_gamma(p.text, t),
    );
    if readable {
        response.on_hover_cursor(CursorIcon::PointingHand);
    } else {
        response.tip("The model doesn't share what it thought");
    }

    let guide_x = rect.left() + space::SM + 6.0;
    reveal(ui, id.with("thoughts"), open && readable, |ui| {
        let top = ui.cursor().top();
        ui.horizontal_top(|ui| {
            ui.add_space(space::LG + space::SM);
            let mut job = LayoutJob::default();
            for (i, line) in text.lines().enumerate() {
                if i > 0 {
                    job.append(
                        "\n",
                        0.0,
                        TextFormat::simple(font::regular(text::MD), p.text_faint),
                    );
                }
                // summaries title each step in bold, which reads better as a heading than as asterisks
                let line = line.trim_end();
                let heading = line.len() > 4 && line.starts_with("**") && line.ends_with("**");
                let (shown, format) = if heading {
                    (
                        &line[2..line.len() - 2],
                        TextFormat::simple(font::medium(text::MD), p.text_muted),
                    )
                } else {
                    (
                        line,
                        TextFormat::simple(font::regular(text::MD), p.text_faint),
                    )
                };
                job.append(shown, 0.0, format);
            }
            ui.vertical(|ui| ui.add(Label::new(job).wrap().selectable(true)));
        });
        // a guide down the left edge ties the thoughts to their summary
        let bottom = ui.min_rect().bottom();
        ui.painter().vline(
            guide_x,
            top..=bottom - space::XS,
            Stroke::new(1.0, p.border),
        );
    });
}

pub(super) fn duration(ms: u64) -> String {
    let secs = (ms / 1000).max(1);
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

// a run of tool calls folds into one summary line, so routine work doesn't crowd the conversation
pub fn activity(ui: &mut Ui, steps: &[&crate::components::chat::ChatItem]) {
    let tools: Vec<&ToolCall> = steps
        .iter()
        .filter_map(|step| match step {
            crate::components::chat::ChatItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("activity");
    let running = tools.iter().any(|t| t.status == ToolStatus::Running);
    let (rect, open) = summary_row(ui, id, running, running, &summary(&tools));
    let guide_x = rect.left() + space::SM + 6.0;
    reveal(ui, id.with("calls"), open, |ui| {
        let top = ui.cursor().top();
        ui.horizontal_top(|ui| {
            ui.add_space(space::LG + space::SM);
            ui.vertical(|ui| {
                // calls inside a group sit close together, unlike the paragraphs around them
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, step) in steps.iter().enumerate() {
                    ui.push_id(i, |ui| match step {
                        crate::components::chat::ChatItem::Tool(tool) => tool_call(ui, tool),
                        crate::components::chat::ChatItem::Reasoning {
                            text,
                            started,
                            ended,
                        } => reasoning(ui, text, *started, *ended),
                        _ => {}
                    });
                }
            });
        });
        // a guide down the left edge ties the expanded calls to their summary
        let bottom = ui.min_rect().bottom();
        ui.painter().vline(
            guide_x,
            top..=bottom - space::XS,
            Stroke::new(1.0, p.border),
        );
    });
}

// a line that opens and closes what's under it, open at first when `open_at_first`, and remembered once clicked
pub fn summary_row(
    ui: &mut Ui,
    id: egui::Id,
    open_at_first: bool,
    running: bool,
    label: &str,
) -> (Rect, bool) {
    let p = theme::palette(ui);
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(open_at_first);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    if response.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    let t = hover_t(ui, &response);
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius::MD),
        p.hover.gamma_multiply(t),
    );
    let y = rect.center().y;
    let turn = spring_bool(ui.ctx(), id.with("caret"), open, Spring::SMOOTH);
    caret(
        ui,
        pos2(rect.left() + space::SM + 6.0, y),
        turn,
        p.text_faint,
    );
    let mut x = rect.left() + space::SM + 12.0 + space::SM;
    if running {
        spinner(ui, pos2(x + icon::MD / 2.0, y), icon::MD, p.text_muted);
        x += icon::MD + space::SM;
    }
    ui.painter().text(
        pos2(x, y),
        Align2::LEFT_CENTER,
        label,
        font::medium(text::MD),
        p.text_muted.lerp_to_gamma(p.text, t),
    );
    response.on_hover_cursor(CursorIcon::PointingHand);
    (rect, open)
}

pub(super) fn summary(tools: &[&ToolCall]) -> String {
    let count = |kind| tools.iter().filter(|t| t.kind == kind).count();
    counts(
        count(ToolKind::Read),
        count(ToolKind::Search),
        count(ToolKind::Edit),
        count(ToolKind::Run),
        count(ToolKind::Mcp),
    )
    .join(" · ")
}

pub(super) fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub(super) fn counts(
    read: usize,
    searched: usize,
    edited: usize,
    ran: usize,
    used: usize,
) -> Vec<String> {
    let mut parts: Vec<String> = [
        (read, "Read", "file", "files"),
        (searched, "Searched", "time", "times"),
        (edited, "Edited", "file", "files"),
        (ran, "Ran", "command", "commands"),
        (used, "Used", "tool", "tools"),
    ]
    .into_iter()
    .filter(|(n, ..)| *n > 0)
    .map(|(n, verb, one, many)| format!("{verb} {}", plural(n, one, many)))
    .collect();
    // "searched 1 time" reads like a counter, where a person would say once
    for part in &mut parts {
        if part == "Searched 1 time" {
            *part = "Searched once".to_owned();
        }
    }
    parts
}

// what a turn did, in the words the tool groups use, for the line its steps fold behind
pub fn turn_summary(
    work: &[&crate::components::chat::ChatItem],
    live: bool,
    took: Option<u64>,
) -> String {
    let what = turn_work(work, live);
    // a turn's length is worth leading with once it's more than a moment
    match took.filter(|ms| !live && *ms >= 1000) {
        Some(ms) => format!("Worked for {} · {what}", duration(ms)),
        None => what,
    }
}

pub(super) fn turn_work(work: &[&crate::components::chat::ChatItem], live: bool) -> String {
    use crate::components::chat::ChatItem;
    let tools: Vec<&ToolCall> = work
        .iter()
        .filter_map(|item| match item {
            ChatItem::Tool(tool) => Some(tool),
            _ => None,
        })
        .collect();
    let count = |kind| tools.iter().filter(|t| t.kind == kind).count();
    // claude's edits come as diffs rather than tool calls, one per file
    let mut edited: Vec<&str> = work
        .iter()
        .filter_map(|item| match item {
            ChatItem::Diff(diff) => Some(diff.path.as_str()),
            _ => None,
        })
        .collect();
    edited.sort_unstable();
    edited.dedup();
    let mut parts = counts(
        count(ToolKind::Read),
        count(ToolKind::Search),
        count(ToolKind::Edit) + edited.len(),
        count(ToolKind::Run),
        count(ToolKind::Mcp),
    );
    let helpers = work
        .iter()
        .filter(|item| matches!(item, ChatItem::Subagent(_)))
        .count();
    if helpers > 0 {
        parts.push(format!("Used {}", plural(helpers, "agent", "agents")));
    }
    if !parts.is_empty() {
        return parts.join(" · ");
    }
    let thought: u64 = work
        .iter()
        .filter_map(|item| match item {
            ChatItem::Reasoning {
                started,
                ended: Some(ended),
                ..
            } => Some(ended.saturating_sub(*started)),
            _ => None,
        })
        .sum();
    if live {
        return "Working".to_owned();
    }
    if thought > 0 {
        return format!("Thought for {}", duration(thought));
    }
    plural(work.len(), "step", "steps")
}

pub(super) const SHIMMER_SECONDS: f32 = 1.6;

// three dots ripple while a soft light sweeps across the label
// what the agent is up to while it works, one word per turn so it reads as personality rather than flicker
pub(super) const THINKING: [&str; 16] = [
    "Thinking",
    "Pondering",
    "Mulling",
    "Noodling",
    "Tinkering",
    "Percolating",
    "Cogitating",
    "Brewing",
    "Untangling",
    "Scheming",
    "Musing",
    "Wrangling",
    "Simmering",
    "Puzzling",
    "Conjuring",
    "Whittling",
];

pub fn thinking(ui: &mut Ui, turn: usize) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let time = ui.input(|i| i.time) as f32;
    let still = motion::reduced(ui.ctx());

    for i in 0..3 {
        // each dot brightens slightly after the one before it, so the light ripples across
        let wave = if still {
            1.0
        } else {
            0.5 + 0.5 * (time * 4.0 - i as f32 * 0.7).sin()
        };
        ui.painter().circle_filled(
            pos2(rect.left() + 4.0 + i as f32 * 9.0, rect.center().y),
            2.5,
            p.text_muted.gamma_multiply(0.35 + 0.65 * wave),
        );
    }

    // the first turn always says thinking, later ones wander through the list
    let word = THINKING[(turn.saturating_sub(1) * 7) % THINKING.len()];
    let label = format!("{word}…");
    let label = label.as_str();
    let count = label.chars().count() as f32;
    // the band starts off the left edge and leaves past the right one, so the sweep never pops in
    // with reduced motion the band waits off the left edge, so no letter glows
    let band = if still {
        -1.0
    } else {
        (time / SHIMMER_SECONDS).fract() * 1.6 - 0.3
    };
    let mut job = LayoutJob::default();
    for (i, ch) in label.chars().enumerate() {
        let at = (i as f32 + 0.5) / count;
        let glow = (1.0 - (at - band).abs() / 0.35).clamp(0.0, 1.0);
        let glow = glow * glow * (3.0 - 2.0 * glow);
        job.append(
            &ch.to_string(),
            0.0,
            TextFormat {
                font_id: font::regular(text::MD),
                color: p.text_faint.lerp_to_gamma(p.text, glow),
                ..Default::default()
            },
        );
    }
    let galley = ui.painter().layout_job(job);
    let pos = pos2(rect.left() + 34.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(pos, galley, p.text_faint);

    if !still {
        ui.ctx().request_repaint();
    }
}
