use std::f32::consts::{FRAC_PI_2, TAU};

use egui::epaint::TextShape;
use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Frame, Label, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextFormat, Ui, pos2, vec2,
};

use super::{
    Decision, DiffKind, FileDiff, PermissionRequest, Subagent, ToolCall, ToolKind, ToolStatus,
};
use crate::components::{Button, hover_t, reveal};
use crate::icons;
use crate::motion::{self, Spring, spring_bool};
use crate::theme::{self, font, icon, radius, space, text};

const ROW: f32 = 32.0;
const DIFF_PREVIEW: usize = 12;

pub fn user_message(ui: &mut Ui, message: &str, attachments: &[String]) {
    let p = theme::palette(ui);
    if !attachments.is_empty() {
        super::attachments::sent(ui, attachments);
    }
    if message.is_empty() {
        return;
    }
    // lay the text out first so the bubble hugs short messages instead of filling the row
    let max = ui.available_width() * 0.8 - 24.0;
    let galley = ui.painter().layout(
        message.to_owned(),
        font::regular(text::MD + 1.0),
        p.text,
        max,
    );
    let width = galley.size().x;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        Frame::new()
            .fill(p.active)
            .corner_radius(radius::XL)
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(width);
                ui.add(Label::new(galley));
            });
    });
}

// one light line per tool call, with its output a click away
pub fn tool_call(ui: &mut Ui, tool: &ToolCall) {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("tool");
    let mut open = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let expandable = tool.output.is_some();

    let sense = if expandable {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), sense);
    if response.clicked() {
        open = !open;
        ui.data_mut(|d| d.insert_temp(id, open));
    }
    let t = if expandable {
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
    let mut x = rect.left() + space::SM;
    let (verb, kind_icon) = match tool.kind {
        ToolKind::Read => ("Read", icons::FILE),
        ToolKind::Edit => ("Edit", icons::PENCIL_SIMPLE),
        ToolKind::Run => ("Run", icons::TERMINAL),
        ToolKind::Search => ("Search", icons::MAGNIFYING_GLASS),
    };
    // finished calls just show what they were, only running and failed ones need a signal
    let center = pos2(x + icon::MD / 2.0, y);
    match tool.status {
        ToolStatus::Running => spinner(ui, center, icon::MD, p.text_muted),
        ToolStatus::Failed => glyph(ui, center, icons::X_CIRCLE, icon::MD, p.danger),
        ToolStatus::Done => glyph(ui, center, kind_icon, icon::MD, p.text_faint),
    }
    x += icon::MD + space::SM;
    let verb = ui.painter().text(
        pos2(x, y),
        Align2::LEFT_CENTER,
        verb,
        font::medium(text::MD),
        p.text_muted,
    );
    let target_x = verb.right() + space::SM;
    let caret_space = if expandable { 28.0 } else { space::SM };
    let target =
        ui.painter()
            .layout_no_wrap(tool.target.clone(), font::mono(text::MONO_SM), p.text_faint);
    let clip = Rect::from_min_max(
        pos2(target_x, rect.top()),
        pos2(rect.right() - caret_space, rect.bottom()),
    );
    ui.painter().with_clip_rect(clip).galley(
        pos2(target_x, y - target.size().y / 2.0),
        target,
        p.text_faint,
    );

    if expandable {
        let turn = spring_bool(ui.ctx(), id.with("caret"), open, Spring::SMOOTH);
        let shown = t.max(turn);
        caret(
            ui,
            pos2(rect.right() - space::MD - 6.0, y),
            turn,
            p.text_faint.gamma_multiply(shown),
        );
        response.on_hover_cursor(CursorIcon::PointingHand);
    }

    if let Some(output) = &tool.output {
        reveal(ui, id.with("output"), open, |ui| {
            ui.add_space(space::XS);
            Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.0, p.border))
                .corner_radius(radius::MD)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ScrollArea::vertical().max_height(220.0).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(output)
                                .font(font::mono(text::MONO_SM))
                                .color(p.text_muted),
                        );
                    });
                });
        });
    }
}

// a run of tool calls folds into one summary line, so routine work doesn't crowd the conversation
pub fn activity(ui: &mut Ui, tools: &[&ToolCall]) {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("activity");
    let running = tools.iter().any(|t| t.status == ToolStatus::Running);
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(running);

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
        summary(tools),
        font::medium(text::MD),
        p.text_muted.lerp_to_gamma(p.text, t),
    );
    response.on_hover_cursor(CursorIcon::PointingHand);

    let guide_x = rect.left() + space::SM + 6.0;
    reveal(ui, id.with("calls"), open, |ui| {
        let top = ui.cursor().top();
        ui.horizontal_top(|ui| {
            ui.add_space(space::LG + space::SM);
            ui.vertical(|ui| {
                // calls inside a group sit close together, unlike the paragraphs around them
                ui.spacing_mut().item_spacing.y = 2.0;
                for (i, tool) in tools.iter().enumerate() {
                    ui.push_id(i, |ui| tool_call(ui, tool));
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

fn summary(tools: &[&ToolCall]) -> String {
    let count = |kind| tools.iter().filter(|t| t.kind == kind).count();
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    [
        (count(ToolKind::Read), "Read", "file", "files"),
        (count(ToolKind::Search), "Searched", "time", "times"),
        (count(ToolKind::Edit), "Edited", "file", "files"),
        (count(ToolKind::Run), "Ran", "command", "commands"),
    ]
    .into_iter()
    .filter(|(n, ..)| *n > 0)
    .map(|(n, verb, one, many)| format!("{verb} {}", plural(n, one, many)))
    .collect::<Vec<_>>()
    .join(" · ")
}

pub fn diff(ui: &mut Ui, diff: &FileDiff) {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("diff");
    let mut expanded = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let added = diff
        .lines
        .iter()
        .filter(|l| l.kind == DiffKind::Add)
        .count();
    let removed = diff
        .lines
        .iter()
        .filter(|l| l.kind == DiffKind::Remove)
        .count();

    card(ui, |ui| {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
        let y = rect.center().y;
        let x = rect.left() + space::MD;
        glyph(
            ui,
            pos2(x + icon::MD / 2.0, y),
            icons::GIT_DIFF,
            icon::MD,
            p.text_faint,
        );
        let path = ui.painter().text(
            pos2(x + icon::MD + space::SM, y),
            Align2::LEFT_CENTER,
            &diff.path,
            font::mono(text::MONO_SM),
            p.text,
        );
        let plus = ui.painter().text(
            pos2(path.right() + space::SM, y),
            Align2::LEFT_CENTER,
            format!("+{added}"),
            font::medium(text::SM),
            p.success,
        );
        ui.painter().text(
            pos2(plus.right() + space::XS, y),
            Align2::LEFT_CENTER,
            format!("−{removed}"),
            font::medium(text::SM),
            p.danger,
        );

        let line = ui.available_rect_before_wrap();
        ui.painter()
            .hline(line.x_range(), line.top(), Stroke::new(1.0, p.border));
        let shown = if expanded {
            diff.lines.len()
        } else {
            diff.lines.len().min(DIFF_PREVIEW)
        };
        let row_h = 18.0;
        let mono = font::mono(text::MONO_SM);
        ui.add_space(space::XS);
        for line in &diff.lines[..shown] {
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::hover());
            let (bg, sign, color) = match line.kind {
                DiffKind::Add => (p.success.gamma_multiply(0.12), "+", p.success),
                DiffKind::Remove => (p.danger.gamma_multiply(0.12), "−", p.danger),
                DiffKind::Context => (Color32::TRANSPARENT, " ", p.text_faint),
            };
            ui.painter().rect_filled(rect, 0.0, bg);
            ui.painter().text(
                pos2(rect.left() + space::MD, rect.center().y),
                Align2::LEFT_CENTER,
                sign,
                mono.clone(),
                color,
            );
            let text_color = if line.kind == DiffKind::Context {
                p.text_muted
            } else {
                p.text
            };
            ui.painter()
                .with_clip_rect(rect.intersect(ui.clip_rect()))
                .text(
                    pos2(rect.left() + space::MD + 16.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    &line.text,
                    mono.clone(),
                    text_color,
                );
        }
        ui.add_space(space::XS);

        let hidden = diff.lines.len() - shown;
        if hidden > 0 || expanded && diff.lines.len() > DIFF_PREVIEW {
            let label = if expanded {
                "Show less".to_owned()
            } else {
                format!("Show {hidden} more lines")
            };
            let (rect, response) =
                ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::click());
            let t = hover_t(ui, &response);
            ui.painter()
                .hline(rect.x_range(), rect.top(), Stroke::new(1.0, p.border));
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                label,
                font::medium(text::SM),
                p.text_faint.lerp_to_gamma(p.text, t),
            );
            if response.on_hover_cursor(CursorIcon::PointingHand).clicked() {
                expanded = !expanded;
                ui.data_mut(|d| d.insert_temp(id, expanded));
            }
        }
    });
}

pub fn permission(ui: &mut Ui, request: &PermissionRequest) -> Option<Decision> {
    match request.decision {
        Some(made) => {
            answered(ui, request, made);
            None
        }
        None => pending(ui, request),
    }
}

// an open request: what the agent wants to do, then the three answers on the right
fn pending(ui: &mut Ui, request: &PermissionRequest) -> Option<Decision> {
    let p = theme::palette(ui);
    let mut decision = None;
    let command = request.title.starts_with("Run");
    let glyph = if command {
        icons::TERMINAL
    } else if request.title.contains("file") {
        icons::PENCIL_SIMPLE
    } else {
        icons::SHIELD_CHECK
    };
    // waiting on the person, so it stands a little forward of the cards around it
    theme::card(ui)
        .stroke(Stroke::new(1.0, p.border_strong))
        .inner_margin(Margin::same(space::MD as i8 + 2))
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(if ui.visuals().dark_mode { 50 } else { 10 }),
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().interact_size.y = 0.0;
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::SM + 2.0;
                let (badge, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
                ui.painter().rect_filled(
                    badge,
                    CornerRadius::same(radius::MD),
                    p.warning.gamma_multiply(0.14),
                );
                ui.painter().text(
                    badge.center(),
                    Align2::CENTER_CENTER,
                    glyph,
                    font::icon(icon::LG),
                    p.warning,
                );
                ui.vertical(|ui| {
                    ui.add_space(1.0);
                    ui.label(
                        RichText::new(request.title.trim_end_matches('?'))
                            .font(font::medium(text::MD + 1.0))
                            .color(p.text),
                    );
                    ui.add_space(2.0);
                    ui.label(
                        RichText::new("Waiting for your approval")
                            .font(font::regular(text::SM + 1.0))
                            .color(p.text_faint),
                    );
                });
            });
            if let Some(detail) = &request.detail {
                ui.add_space(space::MD);
                Frame::new()
                    .fill(p.bg)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(radius::MD)
                    .inner_margin(Margin::symmetric(12, 10))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let mut job = LayoutJob::default();
                        if command {
                            job.append(
                                "$ ",
                                0.0,
                                TextFormat {
                                    font_id: font::mono(text::MONO_SM),
                                    color: p.text_faint,
                                    ..Default::default()
                                },
                            );
                        }
                        job.append(
                            detail,
                            0.0,
                            TextFormat {
                                font_id: font::mono(text::MONO_SM),
                                color: p.text,
                                ..Default::default()
                            },
                        );
                        job.wrap.max_width = ui.available_width();
                        ui.add(Label::new(job));
                    });
            }
            ui.add_space(space::MD);
            // a fixed height row, since a right to left layout would otherwise centre in all the space below
            let row = vec2(ui.available_width(), crate::theme::control::MD);
            let layout = egui::Layout::right_to_left(egui::Align::Center);
            ui.allocate_ui_with_layout(row, layout, |ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                if Button::new("Allow").primary().show(ui).clicked() {
                    decision = Some(Decision::Allow);
                }
                if Button::new("Always allow").show(ui).clicked() {
                    decision = Some(Decision::AlwaysAllow);
                }
                if Button::new("Deny").ghost().show(ui).clicked() {
                    decision = Some(Decision::Deny);
                }
            });
        });
    decision
}

// an answered request settles into one quiet line, like the tool calls around it
fn answered(ui: &mut Ui, request: &PermissionRequest, made: Decision) {
    let p = theme::palette(ui);
    let (glyph, label, color) = match made {
        Decision::Allow => (icons::CHECK_CIRCLE, "Allowed", p.success),
        Decision::AlwaysAllow => (icons::CHECK_CIRCLE, "Always allowed", p.success),
        Decision::Deny => (icons::X_CIRCLE, "Denied", p.danger),
    };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    let mut x = rect.left() + space::SM + icon::MD / 2.0;
    ui.painter().text(
        pos2(x, rect.center().y),
        Align2::CENTER_CENTER,
        glyph,
        font::icon(icon::MD),
        color,
    );
    x += icon::MD / 2.0 + space::SM;
    let verb = ui.painter().text(
        pos2(x, rect.center().y),
        Align2::LEFT_CENTER,
        label,
        font::medium(text::MD),
        p.text_muted,
    );
    if let Some(detail) = &request.detail {
        let first = detail.lines().next().unwrap_or_default();
        let mut job =
            LayoutJob::simple_singleline(first.to_owned(), font::mono(text::MONO_SM), p.text_faint);
        job.wrap = egui::text::TextWrapping {
            max_width: rect.right() - verb.right() - space::SM * 2.0,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = ui.painter().layout_job(job);
        ui.painter().galley(
            pos2(
                verb.right() + space::SM,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            p.text_faint,
        );
    }
}

const SHIMMER_SECONDS: f32 = 1.6;

// three dots ripple while a soft light sweeps across the label
// what the agent is up to while it works, one word per turn so it reads as personality rather than flicker
const THINKING: [&str; 16] = [
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
    let band = if still {
        f32::NAN
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

// a quiet divider where older messages were summarised, so everything above it reads as history
pub fn compaction(ui: &mut Ui, status: ToolStatus) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    let (glyph, label) = match status {
        ToolStatus::Running => (icons::ARROWS_IN_LINE_VERTICAL, "Compacting conversation…"),
        ToolStatus::Done => (icons::ARROWS_IN_LINE_VERTICAL, "Conversation compacted"),
        ToolStatus::Failed => (icons::WARNING, "Couldn't compact the conversation"),
    };
    let still = motion::reduced(ui.ctx());
    let color = if status == ToolStatus::Running && !still {
        let time = ui.input(|i| i.time) as f32;
        ui.ctx().request_repaint();
        p.text_faint
            .lerp_to_gamma(p.text_muted, 0.5 + 0.5 * (time * 3.0).sin())
    } else {
        p.text_faint
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font::regular(text::SM), color);
    let width = icon::SM + space::XS + galley.size().x;
    let left = rect.center().x - width / 2.0;
    let y = rect.center().y;
    ui.painter().text(
        pos2(left + icon::SM / 2.0, y),
        Align2::CENTER_CENTER,
        glyph,
        font::icon(icon::SM),
        color,
    );
    ui.painter().galley(
        pos2(left + icon::SM + space::XS, y - galley.size().y / 2.0),
        galley,
        color,
    );
    let stroke = Stroke::new(1.0, p.border);
    ui.painter()
        .hline(rect.left()..=left - space::MD, y, stroke);
    ui.painter()
        .hline(left + width + space::MD..=rect.right(), y, stroke);
}

// a helper agent as its own card: what it was asked, what it's doing now, and what it found once done
pub fn subagent(ui: &mut Ui, agent: &Subagent) -> Option<String> {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("subagent");
    let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let mut clicked_link = None;
    card(ui, |ui| {
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), 52.0), Sense::click());
        if response.clicked() {
            open = !open;
            ui.data_mut(|d| d.insert_temp(id, open));
        }
        let t = hover_t(ui, &response);
        ui.painter().rect_filled(
            rect.shrink(1.0),
            CornerRadius::same(radius::LG),
            p.hover.gamma_multiply(t * 0.6),
        );
        let y = rect.center().y;
        let tile = Rect::from_center_size(pos2(rect.left() + 12.0 + 14.0, y), vec2(28.0, 28.0));
        ui.painter()
            .rect_filled(tile, CornerRadius::same(radius::MD), theme::tint(p, 0.07));
        crate::components::provider_mark(
            ui.painter(),
            tile.center(),
            icon::MD,
            agent.provider,
            crate::components::provider_color(agent.provider, p.text_muted),
        );

        let left = tile.right() + space::MD;
        let status_x = rect.right() - space::MD - 12.0 - space::MD - icon::MD / 2.0;
        let clip = Rect::from_min_max(
            pos2(left, rect.top()),
            pos2(status_x - icon::MD, rect.bottom()),
        );
        let title = ui.painter().layout_no_wrap(
            if agent.task.is_empty() {
                "Subagent".to_owned()
            } else {
                agent.task.clone()
            },
            font::medium(text::MD),
            p.text,
        );
        ui.painter().with_clip_rect(clip).galley(
            pos2(left, y - 2.0 - title.size().y),
            title,
            p.text,
        );
        let mut meta = Vec::new();
        if !agent.kind.is_empty() {
            meta.push(agent.kind.clone());
        }
        if !agent.steps.is_empty() {
            let n = agent.steps.len();
            meta.push(format!("{n} {}", if n == 1 { "tool" } else { "tools" }));
        }
        if agent.tokens > 0 {
            meta.push(format!(
                "{} tokens",
                super::super::short_tokens(agent.tokens)
            ));
        }
        if agent.background {
            meta.push("background".to_owned());
        }
        ui.painter().with_clip_rect(clip).text(
            pos2(left, y + 2.0),
            Align2::LEFT_TOP,
            meta.join(" · "),
            font::regular(text::SM + 1.0),
            p.text_faint,
        );

        let status = pos2(status_x, y);
        match agent.status {
            ToolStatus::Running => spinner(ui, status, icon::MD, p.text_muted),
            ToolStatus::Done => glyph(ui, status, icons::CHECK_CIRCLE, icon::MD, p.success),
            ToolStatus::Failed => glyph(ui, status, icons::X_CIRCLE, icon::MD, p.danger),
        }
        let turn = spring_bool(ui.ctx(), id.with("caret"), open, Spring::SMOOTH);
        caret(
            ui,
            pos2(rect.right() - space::MD - 6.0, y),
            turn,
            p.text_faint.lerp_to_gamma(p.text_muted, t),
        );
        response.on_hover_cursor(CursorIcon::PointingHand);

        // the line under the header follows the agent: what it's doing, then what it reported
        let line = match agent.status {
            ToolStatus::Running if !agent.activity.is_empty() => {
                Some(format!("{}…", agent.activity))
            }
            ToolStatus::Running => Some("Starting…".to_owned()),
            _ if !open && !agent.result.is_empty() => Some(agent.result.clone()),
            _ => None,
        };
        if let Some(line) = line {
            // laid out at the card's width up front, since a label in a row would stretch the card instead
            let mut job = LayoutJob::simple(
                plain(&line),
                font::regular(text::MD),
                p.text_muted,
                rect.right() - space::LG - left,
            );
            job.wrap.max_rows = 2;
            job.wrap.overflow_character = Some('…');
            let galley = ui.painter().layout_job(job);
            let (row, _) = ui.allocate_exact_size(
                vec2(ui.available_width(), galley.size().y + space::MD),
                Sense::hover(),
            );
            ui.painter()
                .galley(pos2(left, row.top()), galley, p.text_muted);
        }

        reveal(ui, id.with("details"), open, |ui| {
            let line = ui.available_rect_before_wrap();
            ui.painter()
                .hline(line.x_range(), line.top(), Stroke::new(1.0, p.border));
            ui.add_space(space::SM);
            Frame::new()
                .inner_margin(Margin {
                    left: 8,
                    right: 12,
                    top: 0,
                    bottom: 8,
                })
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (i, step) in agent.steps.iter().enumerate() {
                        ui.push_id(i, |ui| tool_call(ui, step));
                    }
                    if !agent.result.is_empty() {
                        ui.add_space(space::SM);
                        ui.horizontal(|ui| {
                            ui.add_space(space::SM);
                            ui.vertical(|ui| {
                                ui.spacing_mut().item_spacing.y = space::SM;
                                if let Some(target) = super::markdown(ui, &agent.result) {
                                    clicked_link = Some(target);
                                }
                            });
                        });
                    }
                });
        });
    });
    clicked_link
}

// a one line preview reads markdown as plain text rather than showing its symbols
fn plain(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("**", "")
        .replace('`', "")
}

fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        add_contents(ui);
    });
}

fn glyph(ui: &Ui, center: Pos2, icon: char, size: f32, color: Color32) {
    ui.painter()
        .text(center, Align2::CENTER_CENTER, icon, font::icon(size), color);
}

fn rotated(ui: &Ui, center: Pos2, icon: char, size: f32, angle: f32, color: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(icon.to_string(), font::icon(size), color);
    ui.painter().add(
        TextShape::new(center - galley.size() / 2.0, galley, color)
            .with_angle_and_anchor(angle, Align2::CENTER_CENTER),
    );
}

fn spinner(ui: &Ui, center: Pos2, size: f32, color: Color32) {
    if motion::reduced(ui.ctx()) {
        glyph(ui, center, icons::SPINNER, size, color);
        return;
    }
    let angle = (ui.input(|i| i.time) as f32 * TAU * 0.8) % TAU;
    rotated(ui, center, icons::SPINNER, size, angle, color);
    ui.ctx().request_repaint();
}

fn caret(ui: &Ui, center: Pos2, turn: f32, color: Color32) {
    rotated(
        ui,
        center,
        icons::CARET_RIGHT,
        12.0,
        turn * FRAC_PI_2,
        color,
    );
}
