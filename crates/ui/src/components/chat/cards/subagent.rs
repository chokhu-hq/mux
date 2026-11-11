use super::*;

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
        ui.painter().rect_filled(
            tile,
            CornerRadius::same(radius::MD),
            theme::tint(p, theme::fill::QUIET),
        );
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
                mux_core::clip_lines(&agent.task).into_owned()
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
            meta.push(mux_core::clip_lines(&agent.kind).into_owned());
        }
        if !agent.steps.is_empty() {
            let n = agent.steps.len();
            meta.push(format!("{n} {}", if n == 1 { "tool" } else { "tools" }));
        }
        if agent.tokens > 0 {
            meta.push(format!(
                "{} tokens",
                crate::components::short_tokens(agent.tokens)
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
                                if let Some(target) =
                                    crate::components::chat::markdown(ui, &agent.result)
                                {
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
