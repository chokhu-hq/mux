use std::f32::consts::{FRAC_PI_2, TAU};

use egui::epaint::TextShape;
use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Frame, Label, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextFormat, Ui, pos2, vec2,
};

use super::{Decision, DiffKind, FileDiff, PermissionRequest, ToolCall, ToolKind, ToolStatus};
use crate::components::{Button, Size, hover_t, reveal};
use crate::icons;
use crate::motion::{self, Spring, spring_bool};
use crate::theme::{self, font, icon, radius, space, text};

const ROW: f32 = 32.0;
const DIFF_PREVIEW: usize = 12;

pub fn user_message(ui: &mut Ui, message: &str) {
    let p = theme::palette(ui);
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
    let p = theme::palette(ui);
    let mut decision = None;
    // only an open request needs to stand out, an answered one settles into a plain card
    let pending = spring_bool(
        ui.ctx(),
        ui.make_persistent_id("pending"),
        request.decision.is_none(),
        Spring::SMOOTH,
    );
    Frame::new()
        .fill(p.warning.gamma_multiply(0.06 * pending))
        .stroke(Stroke::new(
            1.0,
            p.border
                .lerp_to_gamma(p.warning.gamma_multiply(0.45), pending),
        ))
        .corner_radius(radius::LG)
        .inner_margin(Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // the title row holds no controls, so it shouldn't reserve a control's height
            ui.spacing_mut().interact_size.y = 0.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(icons::WARNING.to_string())
                        .font(font::icon(icon::MD))
                        .color(p.warning),
                );
                ui.label(RichText::new(&request.title).font(font::medium(text::MD)));
            });
            if let Some(detail) = &request.detail {
                ui.add_space(space::XS);
                Frame::new()
                    .fill(p.surface)
                    .stroke(Stroke::new(1.0, p.border))
                    .corner_radius(radius::MD)
                    .inner_margin(Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            RichText::new(detail)
                                .font(font::mono(text::MONO_SM))
                                .color(p.text),
                        );
                    });
            }
            ui.add_space(space::XS);
            match request.decision {
                None => {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::XS + 2.0;
                        if Button::new("Allow")
                            .primary()
                            .size(Size::Sm)
                            .show(ui)
                            .clicked()
                        {
                            decision = Some(Decision::Allow);
                        }
                        if Button::new("Always allow")
                            .size(Size::Sm)
                            .show(ui)
                            .clicked()
                        {
                            decision = Some(Decision::AlwaysAllow);
                        }
                        if Button::new("Deny")
                            .ghost()
                            .size(Size::Sm)
                            .show(ui)
                            .clicked()
                        {
                            decision = Some(Decision::Deny);
                        }
                    });
                }
                Some(made) => {
                    let (icon, label, color) = match made {
                        Decision::Allow => (icons::CHECK, "Allowed", p.success),
                        Decision::AlwaysAllow => (icons::CHECK, "Always allowed", p.success),
                        Decision::Deny => (icons::X, "Denied", p.danger),
                    };
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(icon.to_string())
                                .font(font::icon(icon::SM))
                                .color(color),
                        );
                        ui.label(
                            RichText::new(label)
                                .font(font::regular(text::SM))
                                .color(p.text_muted),
                        );
                    });
                }
            }
        });
    decision
}

const SHIMMER_SECONDS: f32 = 1.6;

// three dots ripple while a soft light sweeps across the label
pub fn thinking(ui: &mut Ui) {
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

    let label = "Thinking";
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

fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    let p = theme::palette(ui);
    Frame::new()
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(radius::LG)
        .show(ui, |ui| {
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
