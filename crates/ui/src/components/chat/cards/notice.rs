use super::*;

// a limit the agent hit, with when mux carries on by itself, and a banked reset to use instead when there is one.
// returns true once using a reset is confirmed
pub fn usage_limit(
    ui: &mut Ui,
    message: &str,
    retry_at: Option<u64>,
    reset: Option<&crate::components::chat::ResetOffer>,
) -> bool {
    let mut use_reset = false;
    let p = theme::palette(ui);
    theme::card(ui)
        .inner_margin(Margin::same(space::MD as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(icons::CLOCK)
                        .font(font::icon(icon::MD))
                        .color(p.text_muted),
                );
                ui.label(
                    RichText::new("Usage limit reached")
                        .font(font::medium(text::MD))
                        .color(p.text),
                );
            });
            ui.add_space(space::SM);
            ui.add(
                Label::new(RichText::new(message).color(p.text_muted))
                    .wrap()
                    .selectable(true),
            );
            if let Some(at) = retry_at {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs());
                let left = at.saturating_sub(now);
                let when = match left {
                    0..=90 => "shortly after the limit resets".to_owned(),
                    91..=3599 => format!("in about {} min", left.div_ceil(60)),
                    _ => format!("in about {} h {} min", left / 3600, left % 3600 / 60),
                };
                ui.add_space(space::SM);
                let status = format!(
                    "Mux will retry automatically {when}. Sending a message takes over instead."
                );
                ui.add(
                    Label::new(
                        RichText::new(status)
                            .font(font::regular(text::SM))
                            .color(p.text_muted),
                    )
                    .wrap()
                    .selectable(true),
                );
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs(60));
            }
            if let Some(reset) = reset {
                use_reset = reset_row(ui, reset);
            }
        });
    use_reset
}

// a banked reset, used only after a second click, since it can't be taken back
pub(super) fn reset_row(ui: &mut Ui, reset: &crate::components::chat::ResetOffer) -> bool {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("reset-confirm");
    let mut asking = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    let mut confirmed = false;
    if let Some(note) = &reset.note {
        ui.add_space(space::SM);
        ui.add(
            Label::new(
                RichText::new(note)
                    .font(font::regular(text::SM))
                    .color(p.text_muted),
            )
            .wrap(),
        );
    }
    if reset.left == 0 && !reset.pending {
        return false;
    }
    ui.add_space(space::MD);
    if reset.pending {
        ui.horizontal(|ui| {
            spinner(
                ui,
                ui.cursor().left_center() + vec2(icon::SM / 2.0, 0.0),
                icon::SM,
                p.text_muted,
            );
            ui.add_space(icon::SM + space::SM);
            ui.label(
                RichText::new("Using a reset…")
                    .font(font::regular(text::SM))
                    .color(p.text_muted),
            );
        });
        return false;
    }
    let left = reset.left;
    if asking {
        ui.add(
            Label::new(
                RichText::new(format!(
                    "This uses 1 of your {left} {} and lifts the limit now. It can't be undone.",
                    if left == 1 { "reset" } else { "resets" }
                ))
                .font(font::regular(text::SM))
                .color(p.text),
            )
            .wrap(),
        );
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            if Button::new("Use reset").primary().show(ui).clicked() {
                confirmed = true;
                asking = false;
            }
            if Button::new("Cancel").ghost().show(ui).clicked() {
                asking = false;
            }
        });
    } else if Button::new(format!("Use a reset ({left} left)"))
        .show(ui)
        .clicked()
    {
        asking = true;
    }
    ui.data_mut(|d| d.insert_temp(id, asking));
    confirmed
}

// a quiet divider where older messages were summarised, so everything above it reads as history
// where the chat moved to another agent, as a divider like a compaction's
pub fn switched(ui: &mut Ui, from: &str, to: &str) {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
    response
        .on_hover_text("The new agent was given the conversation so far with your next message");
    divider(
        ui,
        rect,
        icons::ARROWS_LEFT_RIGHT,
        &format!("Switched from {from} to {to}"),
        p.text_faint,
    );
}

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
    divider(ui, rect, glyph, label, color);
}

// a centred icon and label with a rule either side
pub(super) fn divider(ui: &Ui, rect: egui::Rect, glyph: char, label: &str, color: egui::Color32) {
    let p = theme::palette(ui);
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

// a failure the agent or mux reported, as a card of its own so it can't pass for part of a reply
pub fn error(ui: &mut Ui, message: &str) -> egui::Response {
    let p = theme::palette(ui);
    Frame::new()
        .fill(p.danger.gamma_multiply(0.08))
        .stroke(Stroke::new(1.0, p.danger.gamma_multiply(0.35)))
        .corner_radius(radius::MD)
        .inner_margin(Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                ui.label(
                    RichText::new(icons::WARNING)
                        .font(font::icon(icon::MD))
                        .color(p.danger),
                );
                ui.add(
                    Label::new(
                        RichText::new(message)
                            .font(font::regular(text::MD))
                            .color(p.text),
                    )
                    .wrap()
                    .selectable(true),
                );
            });
        })
        .response
}

// the agent trying again after its model failed to answer, which would otherwise look like it's stuck
pub fn retry(ui: &mut Ui, reason: &str, attempt: u32, of: Option<u32>, status: ToolStatus) {
    let reason = mux_core::clip_lines(reason);
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    let y = rect.center().y;
    let center = pos2(rect.left() + space::SM + icon::MD / 2.0, y);
    let tries = |n: u32| if n == 1 { "try" } else { "tries" };
    let label = match status {
        ToolStatus::Running => {
            spinner(ui, center, icon::MD, p.text_muted);
            match of {
                Some(of) => format!("{reason}, trying again ({attempt} of {of})"),
                None => format!("{reason}, trying again"),
            }
        }
        ToolStatus::Done => {
            glyph(ui, center, icons::ARROW_CLOCKWISE, icon::MD, p.text_faint);
            format!("Recovered after {attempt} {}", tries(attempt))
        }
        ToolStatus::Failed => {
            glyph(ui, center, icons::WARNING, icon::MD, p.text_faint);
            format!("Gave up after {attempt} {}", tries(attempt))
        }
    };
    let clip = Rect::from_min_max(pos2(center.x, rect.top()), rect.max);
    ui.painter().with_clip_rect(clip).text(
        pos2(center.x + icon::MD / 2.0 + space::SM, y),
        Align2::LEFT_CENTER,
        label,
        font::regular(text::MD),
        p.text_muted,
    );
}
