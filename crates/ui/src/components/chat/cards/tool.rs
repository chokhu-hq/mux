use super::*;

// one light line per tool call, with its output a click away
pub fn tool_call(ui: &mut Ui, tool: &ToolCall) {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("tool");
    let mut open = ui.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let (preview, more) = one_line(&tool.target);
    // a command over several lines can always be opened, so the rest of it is never out of reach
    let expandable = tool.output.is_some() || more > 0;

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
        ToolKind::Mcp => ("Used", icons::PLUGS_CONNECTED),
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
    let mut target_x = verb.right() + space::SM;
    // a command still running after the agent moved on is marked, so it doesn't read as finished
    if tool.background {
        let tag = ui.painter().layout_no_wrap(
            "background".to_owned(),
            font::regular(text::SM),
            p.text_faint,
        );
        let pill = Rect::from_min_size(
            pos2(target_x, y - tag.size().y / 2.0 - 2.0),
            vec2(tag.size().x + space::SM * 2.0, tag.size().y + 4.0),
        );
        ui.painter().rect_filled(
            pill,
            CornerRadius::same(radius::SM),
            theme::tint(p, theme::fill::QUIET),
        );
        ui.painter().galley(
            pos2(pill.left() + space::SM, pill.top() + 2.0),
            tag,
            p.text_faint,
        );
        target_x = pill.right() + space::SM;
    }
    let caret_space = if expandable { 28.0 } else { space::SM };
    // the row is one line tall, so a longer command is run together onto it and cut off at the edge
    let target = ui
        .painter()
        .layout_no_wrap(preview, font::mono(text::MONO_SM), p.text_faint);
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

    crate::components::area_menu(ui, rect, id.with("menu"), |ui| {
        let what = if tool.kind == ToolKind::Run {
            "Copy command"
        } else {
            "Copy"
        };
        crate::components::copy_item(ui, what, tool.target.trim());
        if let Some(output) = &tool.output {
            crate::components::copy_item(ui, "Copy output", output);
        }
    });

    if expandable {
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
                        if more > 0 {
                            ui.label(
                                RichText::new(tool.target.trim())
                                    .font(font::mono(text::MONO_SM))
                                    .color(p.text),
                            );
                        }
                        if let Some(output) = &tool.output {
                            if more > 0 {
                                ui.add_space(space::SM);
                                let line = ui.available_rect_before_wrap();
                                ui.painter().hline(
                                    line.x_range(),
                                    line.top(),
                                    Stroke::new(1.0, p.border),
                                );
                                ui.add_space(space::SM);
                            }
                            ui.label(
                                RichText::new(output)
                                    .font(font::mono(text::MONO_SM))
                                    .color(p.text_muted),
                            );
                        }
                    });
                });
        });
    }
    // pictures a tool gave back are the point of calling it, so they show without opening the row
    if !tool.images.is_empty() {
        ui.add_space(space::XS);
        crate::components::chat::attachments::previews(ui, &tool.images);
    }
}

// the text's lines run together with single spaces, and how many lines there were beyond the first
pub(super) fn one_line(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let joined = lines.join(" ");
    (
        mux_core::clip_lines(&joined).into_owned(),
        lines.len().saturating_sub(1),
    )
}

#[cfg(test)]
mod tool_row_tests {
    use crate::components::chat::cards::{one_line, permission};
    use crate::components::chat::{Decision, PermissionRequest};
    use egui::vec2;

    #[test]
    fn long_commands_run_together_onto_one_line() {
        let script = "bun -e '\n  import { loadKnowledge } from \"./src/knowledge.ts\";\n\n  console.log(k.docs.size);\n'";
        assert_eq!(
            one_line(script),
            (
                "bun -e ' import { loadKnowledge } from \"./src/knowledge.ts\"; console.log(k.docs.size); '".to_owned(),
                3
            )
        );
        assert_eq!(one_line("cargo test"), ("cargo test".to_owned(), 0));
        assert_eq!(one_line("\n  ls -la  \n"), ("ls -la".to_owned(), 0));
    }

    fn request() -> PermissionRequest {
        PermissionRequest {
            title: "Run a command?".into(),
            detail: Some("rm -rf build".into()),
            decision: None,
        }
    }

    // one frame at `time` seconds with these pointer events, returning what the card was answered with
    fn frame(ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> Option<Decision> {
        let input = egui::RawInput {
            time: Some(time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                vec2(480.0, 240.0),
            )),
            events,
            ..Default::default()
        };
        let mut answer = None;
        ctx.run_ui(input, |ui| answer = permission(ui, &request()))
            .drop_without_applying_deltas();
        answer
    }

    fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        }
    }

    // clicks every few points across the window at `time`, returning where a click answered the card
    fn sweep(ctx: &egui::Context, time: f64) -> Option<egui::Pos2> {
        for y in (0..240).step_by(6) {
            for x in (0..480).step_by(6) {
                let at = egui::pos2(x as f32, y as f32);
                frame(
                    ctx,
                    time,
                    vec![egui::Event::PointerMoved(at), button(at, true)],
                );
                if frame(ctx, time, vec![button(at, false)]).is_some() {
                    return Some(at);
                }
            }
        }
        None
    }

    #[test]
    fn a_new_request_cant_be_answered_until_it_has_been_seen_a_moment() {
        let ctx = egui::Context::default();
        frame(&ctx, 0.0, Vec::new());
        assert_eq!(sweep(&ctx, 0.3), None, "nothing answers it while it's new");
        let allow = sweep(&ctx, 1.0).expect("its answers work once it's settled");

        // a press made while it was new, let go of once it's settled, still isn't an answer
        let ctx = egui::Context::default();
        frame(&ctx, 0.0, Vec::new());
        frame(
            &ctx,
            0.3,
            vec![egui::Event::PointerMoved(allow), button(allow, true)],
        );
        assert_eq!(frame(&ctx, 1.0, vec![button(allow, false)]), None);
    }

    fn limit(ctx: &egui::Context, time: f64, events: Vec<egui::Event>) -> bool {
        let input = egui::RawInput {
            time: Some(time),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                vec2(480.0, 260.0),
            )),
            events,
            ..Default::default()
        };
        let offer = crate::components::chat::ResetOffer {
            left: 2,
            pending: false,
            note: None,
        };
        let mut used = false;
        ctx.run_ui(input, |ui| {
            used = crate::components::chat::cards::usage_limit(
                ui,
                "You've hit your usage limit.",
                None,
                Some(&offer),
            )
        })
        .drop_without_applying_deltas();
        used
    }

    #[test]
    fn a_reset_is_only_spent_after_it_is_confirmed() {
        // clicking everywhere on one card finds where the confirm button ends up
        let ctx = egui::Context::default();
        limit(&ctx, 0.0, Vec::new());
        let mut confirm = None;
        'sweep: for y in (0..260).step_by(4) {
            for x in (0..480).step_by(4) {
                let at = egui::pos2(x as f32, y as f32);
                limit(
                    &ctx,
                    1.0,
                    vec![egui::Event::PointerMoved(at), button(at, true)],
                );
                if limit(&ctx, 1.0, vec![button(at, false)]) {
                    confirm = Some(at);
                    break 'sweep;
                }
            }
        }
        let confirm = confirm.expect("a reset can be used");
        // on a fresh card the same single click spends nothing, since the confirm button isn't there yet
        let ctx = egui::Context::default();
        limit(&ctx, 0.0, Vec::new());
        limit(
            &ctx,
            1.0,
            vec![egui::Event::PointerMoved(confirm), button(confirm, true)],
        );
        assert!(!limit(&ctx, 1.0, vec![button(confirm, false)]));
    }
}
