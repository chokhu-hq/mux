use super::*;

pub fn permission(ui: &mut Ui, request: &PermissionRequest) -> Option<Decision> {
    match request.decision {
        Some(made) => {
            answered(ui, request, made);
            None
        }
        None => pending(ui, request),
    }
}

// how long a new request's answers stay off, so a click meant for something else, like the spot where the
// request just appeared or moved to, can't approve it
pub(super) const ARMING: f64 = 0.6;

// an open request: what the agent wants to do, then the three answers on the right
pub(super) fn pending(ui: &mut Ui, request: &PermissionRequest) -> Option<Decision> {
    let p = theme::palette(ui);
    let mut decision = None;
    let now = ui.input(|i| i.time);
    let seen = ui
        .scope_id()
        .with(("permission-shown", &request.title, &request.detail));
    let shown = ui.data_mut(|d| *d.get_temp_mut_or_insert_with(seen, || now));
    let armed = now - shown >= ARMING;
    if !armed {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_secs_f64(ARMING - (now - shown)));
    }
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
                // a press that began while they were off never becomes a click once they're on
                answers(ui, armed, &mut decision);
            });
        });
    decision
}

pub(super) fn answers(ui: &mut Ui, armed: bool, decision: &mut Option<Decision>) {
    if Button::new("Allow")
        .primary()
        .enabled(armed)
        .show(ui)
        .clicked()
    {
        *decision = Some(Decision::Allow);
    }
    if Button::new("Always allow")
        .enabled(armed)
        .show(ui)
        .clicked()
    {
        *decision = Some(Decision::AlwaysAllow);
    }
    if Button::new("Deny")
        .ghost()
        .enabled(armed)
        .show(ui)
        .clicked()
    {
        *decision = Some(Decision::Deny);
    }
}

// an answered request settles into one quiet line, like the tool calls around it
pub(super) fn answered(ui: &mut Ui, request: &PermissionRequest, made: Decision) {
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
