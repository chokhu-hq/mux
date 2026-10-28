use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Margin, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use mux_core::protocol::{Question, TodoStatus, TodoStep};

use super::markdown;
use crate::components::{Button, TextInput, TooltipExt, hover_t};
use crate::icons;
use crate::motion;
use crate::theme::{self, font, icon, radius, space, text};

// what's been picked so far for each question, and anything typed instead
#[derive(Clone, Default)]
struct Draft {
    picked: Vec<Vec<bool>>,
    typed: Vec<String>,
}

// a card standing a little forward of the chat, since it's waiting on the person
fn forward(ui: &Ui) -> egui::Frame {
    let p = theme::palette(ui);
    theme::card(ui)
        .stroke(Stroke::new(1.0, p.border_strong))
        .inner_margin(Margin::same(space::MD as i8 + 2))
        .shadow(egui::Shadow {
            offset: [0, 2],
            blur: 10,
            spread: 0,
            color: Color32::from_black_alpha(if ui.visuals().dark_mode { 50 } else { 10 }),
        })
}

fn heading(ui: &mut Ui, glyph: char, tint: Color32, title: &str, detail: &str) {
    let p = theme::palette(ui);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM + 2.0;
        let (badge, _) = ui.allocate_exact_size(vec2(32.0, 32.0), Sense::hover());
        ui.painter().rect_filled(
            badge,
            CornerRadius::same(radius::MD),
            tint.gamma_multiply(0.14),
        );
        ui.painter().text(
            badge.center(),
            Align2::CENTER_CENTER,
            glyph,
            font::icon(icon::LG),
            tint,
        );
        ui.vertical(|ui| {
            ui.add_space(1.0);
            ui.label(
                RichText::new(title)
                    .font(font::medium(text::MD + 1.0))
                    .color(p.text),
            );
            ui.add_space(2.0);
            ui.label(
                RichText::new(detail)
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_faint),
            );
        });
    });
}

// the agent's question with its options, returning the answers once they're sent
pub fn question(
    ui: &mut Ui,
    questions: &[Question],
    answers: Option<&[Vec<String>]>,
) -> Option<Vec<Vec<String>>> {
    let p = theme::palette(ui);
    if let Some(answers) = answers {
        // once answered it shrinks to one row like a decided permission, cut short with the rest a hover away
        let said: Vec<String> = answers.iter().map(|a| a.join(", ")).collect();
        let said = said.join(" · ");
        let (glyph, label, color) = if answers.is_empty() {
            (icons::X_CIRCLE, "Not answered", p.text_faint)
        } else {
            (icons::CHECK_CIRCLE, "Answered", p.success)
        };
        let (rect, response) = ui.allocate_exact_size(
            vec2(ui.available_width(), super::cards::ROW),
            Sense::hover(),
        );
        let y = rect.center().y;
        let mut x = rect.left() + space::SM + icon::MD / 2.0;
        ui.painter().text(
            pos2(x, y),
            Align2::CENTER_CENTER,
            glyph,
            font::icon(icon::MD),
            color,
        );
        x += icon::MD / 2.0 + space::SM;
        let verb = ui.painter().text(
            pos2(x, y),
            Align2::LEFT_CENTER,
            label,
            font::medium(text::MD),
            p.text_muted,
        );
        if !said.is_empty() {
            // a typed answer can run over several lines, which the row puts together
            let line = said.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut job = egui::text::LayoutJob::simple_singleline(
                line,
                font::regular(text::MD),
                p.text_faint,
            );
            job.wrap = egui::text::TextWrapping {
                max_width: rect.right() - verb.right() - space::SM * 2.0,
                max_rows: 1,
                break_anywhere: true,
                overflow_character: Some('…'),
            };
            let galley = ui.painter().layout_job(job);
            let cut = galley.elided;
            ui.painter().galley(
                pos2(verb.right() + space::SM, y - galley.size().y / 2.0),
                galley,
                p.text_faint,
            );
            if cut {
                response.tip(said);
            }
        }
        return None;
    }
    let id = ui.make_persistent_id("question");
    let mut draft: Draft = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    draft.picked.resize_with(questions.len(), Vec::new);
    draft.typed.resize_with(questions.len(), String::new);
    let mut sent = None;
    forward(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        let title = if questions.len() == 1 {
            "The agent has a question"
        } else {
            "The agent has some questions"
        };
        heading(
            ui,
            icons::CHAT_CIRCLE,
            p.accent,
            title,
            "It's waiting for your answer",
        );
        for (q, question) in questions.iter().enumerate() {
            ui.push_id(q, |ui| {
                ui.add_space(space::MD);
                if !question.header.is_empty() {
                    ui.label(
                        RichText::new(&question.header)
                            .font(font::medium(text::SM))
                            .color(p.text_faint),
                    );
                    ui.add_space(space::XS);
                }
                ui.add(
                    egui::Label::new(
                        RichText::new(&question.question)
                            .font(font::medium(text::MD + 1.0))
                            .color(p.text),
                    )
                    .wrap(),
                );
                ui.add_space(space::SM);
                let picked = &mut draft.picked[q];
                picked.resize(question.options.len(), false);
                for (o, option) in question.options.iter().enumerate() {
                    if choice(
                        ui,
                        o,
                        &option.label,
                        &option.description,
                        picked[o],
                        question.multiple,
                    ) {
                        if question.multiple {
                            picked[o] = !picked[o];
                        } else {
                            picked.iter_mut().enumerate().for_each(|(i, p)| *p = i == o);
                            draft.typed[q].clear();
                        }
                    }
                }
                if question.other {
                    ui.add_space(space::XS);
                    let hint = if question.options.is_empty() {
                        "Type your answer"
                    } else {
                        "Or type your own answer"
                    };
                    let before = draft.typed[q].clone();
                    TextInput::new(&mut draft.typed[q])
                        .placeholder(hint)
                        .show(ui);
                    // typing an answer replaces a single pick
                    if !question.multiple && draft.typed[q] != before && !draft.typed[q].is_empty()
                    {
                        draft.picked[q].iter_mut().for_each(|p| *p = false);
                    }
                }
            });
        }
        ui.add_space(space::MD);
        let answers: Vec<Vec<String>> = questions
            .iter()
            .enumerate()
            .map(|(q, question)| {
                let mut picks: Vec<String> = question
                    .options
                    .iter()
                    .zip(&draft.picked[q])
                    .filter(|(_, on)| **on)
                    .map(|(o, _)| o.label.clone())
                    .collect();
                if !draft.typed[q].trim().is_empty() {
                    picks.push(draft.typed[q].trim().to_owned());
                }
                picks
            })
            .collect();
        let ready = answers.iter().all(|a| !a.is_empty());
        ui.horizontal(|ui| {
            if Button::new("Answer")
                .primary()
                .enabled(ready)
                .show(ui)
                .clicked()
            {
                sent = Some(answers);
            }
        });
    });
    ui.data_mut(|d| d.insert_temp(id, draft));
    sent
}

// one option row, a round mark for a single pick and a square one for several
fn choice(ui: &mut Ui, index: usize, label: &str, detail: &str, on: bool, multiple: bool) -> bool {
    let p = theme::palette(ui);
    let height = if detail.is_empty() { 34.0 } else { 46.0 };
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::click());
    let t = hover_t(ui, &response);
    let shown = motion::spring_bool(
        ui.ctx(),
        response.id.with(("on", index)),
        on,
        motion::Spring::QUICK,
    );
    let fill = Color32::TRANSPARENT
        .lerp_to_gamma(theme::tint(p, theme::fill::HOVER), t)
        .lerp_to_gamma(p.accent.gamma_multiply(0.12), shown);
    ui.painter()
        .rect_filled(rect, CornerRadius::same(radius::MD), fill);
    let mark = pos2(rect.left() + space::SM + icon::MD / 2.0, rect.top() + 17.0);
    let glyph = match (multiple, on) {
        (true, true) => icons::CHECK_CIRCLE,
        (false, true) => icons::CHECK_CIRCLE,
        _ => icons::CIRCLE,
    };
    let color = p.text_faint.lerp_to_gamma(p.accent, shown);
    ui.painter().text(
        mark,
        Align2::CENTER_CENTER,
        glyph,
        font::icon(icon::MD),
        color,
    );
    let x = mark.x + icon::MD / 2.0 + space::SM;
    ui.painter().text(
        pos2(x, rect.top() + 17.0),
        Align2::LEFT_CENTER,
        label,
        font::medium(text::MD),
        p.text,
    );
    if !detail.is_empty() {
        let mut job = egui::text::LayoutJob::simple_singleline(
            detail.to_owned(),
            font::regular(text::SM + 1.0),
            p.text_faint,
        );
        job.wrap = egui::text::TextWrapping {
            max_width: rect.right() - space::SM - x,
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = ui.painter().layout_job(job);
        ui.painter()
            .galley(pos2(x, rect.top() + 28.0), galley, p.text_faint);
    }
    response.on_hover_cursor(CursorIcon::PointingHand).clicked()
}

pub enum PlanAction {
    Implement,
    Link(String),
}

// a proposed plan, rendered, with the step that carries it out when it's the one in play
pub fn plan(ui: &mut Ui, text: &str, actionable: bool) -> Option<PlanAction> {
    let p = theme::palette(ui);
    let mut action = None;
    let frame = if actionable {
        forward(ui)
    } else {
        theme::card(ui).inner_margin(Margin::same(space::MD as i8 + 2))
    };
    frame.show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        let detail = if actionable {
            "Read it over, then carry it out or reply to change it"
        } else {
            "Proposed plan"
        };
        heading(ui, icons::LIST_CHECKS, p.accent, "Plan", detail);
        ui.add_space(space::MD);
        let line = ui.available_rect_before_wrap();
        ui.painter()
            .hline(line.x_range(), line.top(), Stroke::new(1.0, p.border));
        ui.add_space(space::MD);
        if let Some(target) = markdown(ui, text) {
            action = Some(PlanAction::Link(target));
        }
        if actionable {
            ui.add_space(space::MD);
            ui.horizontal(|ui| {
                if Button::new("Implement plan").primary().show(ui).clicked() {
                    action = Some(PlanAction::Implement);
                }
            });
        }
    });
    action
}

// the agent's checklist for the turn, ticking off as it goes
pub fn todos(ui: &mut Ui, steps: &[TodoStep]) {
    let p = theme::palette(ui);
    let done = steps
        .iter()
        .filter(|s| s.status == TodoStatus::Done)
        .count();
    theme::card(ui)
        .inner_margin(Margin::symmetric(space::MD as i8, space::SM as i8 + 2))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
            let y = rect.center().y;
            ui.painter().text(
                pos2(rect.left() + icon::MD / 2.0, y),
                Align2::CENTER_CENTER,
                icons::LIST_CHECKS,
                font::icon(icon::MD),
                p.text_muted,
            );
            ui.painter().text(
                pos2(rect.left() + icon::MD + space::SM, y),
                Align2::LEFT_CENTER,
                "Tasks",
                font::medium(text::MD),
                p.text,
            );
            ui.painter().text(
                pos2(rect.right(), y),
                Align2::RIGHT_CENTER,
                format!("{done} of {}", steps.len()),
                font::regular(text::SM + 1.0),
                p.text_faint,
            );
            ui.add_space(space::XS);
            for step in steps {
                let (rect, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                let y = rect.center().y;
                let center = pos2(rect.left() + icon::MD / 2.0, y);
                match step.status {
                    TodoStatus::Done => {
                        ui.painter().text(
                            center,
                            Align2::CENTER_CENTER,
                            icons::CHECK_CIRCLE,
                            font::icon(icon::MD),
                            p.success,
                        );
                    }
                    TodoStatus::Active => {
                        // the step being worked on pulses, the rest sit still
                        let time = ui.input(|i| i.time) as f32;
                        let pulse = if motion::reduced(ui.ctx()) {
                            1.0
                        } else {
                            0.6 + 0.4 * (time * 3.0).sin().abs()
                        };
                        ui.painter()
                            .circle_stroke(center, 5.5, Stroke::new(1.5, p.accent));
                        ui.painter()
                            .circle_filled(center, 2.5, p.accent.gamma_multiply(pulse));
                        ui.ctx().request_repaint();
                    }
                    TodoStatus::Pending => {
                        ui.painter()
                            .circle_stroke(center, 5.5, Stroke::new(1.2, p.text_faint));
                    }
                }
                let color = match step.status {
                    TodoStatus::Done => p.text_faint,
                    TodoStatus::Active => p.text,
                    TodoStatus::Pending => p.text_muted,
                };
                let mut job = egui::text::LayoutJob::simple_singleline(
                    step.text.clone(),
                    font::regular(text::MD),
                    color,
                );
                job.wrap = egui::text::TextWrapping {
                    max_width: rect.right() - rect.left() - icon::MD - space::SM,
                    max_rows: 1,
                    break_anywhere: true,
                    overflow_character: Some('…'),
                };
                if step.status == TodoStatus::Done
                    && let Some(section) = job.sections.first_mut()
                {
                    section.format.strikethrough = Stroke::new(1.0, p.text_faint);
                }
                let galley = ui.painter().layout_job(job);
                ui.painter().galley(
                    pos2(
                        rect.left() + icon::MD + space::SM,
                        y - galley.size().y / 2.0,
                    ),
                    galley,
                    color,
                );
            }
        });
}
