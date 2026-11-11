use super::*;

// one of the person's messages, returning true when they asked to rewind the chat to it
pub fn user_message(
    ui: &mut Ui,
    message: &str,
    attachments: &[String],
    queued: bool,
    rewindable: bool,
) -> bool {
    let p = theme::palette(ui);
    // a message still waiting its turn sits back a little until the agent picks it up
    let waiting = spring_bool(
        ui.ctx(),
        ui.make_persistent_id("queued"),
        queued,
        Spring::SMOOTH,
    );
    let opacity = ui.opacity();
    ui.multiply_opacity(1.0 - 0.4 * waiting);
    if !attachments.is_empty() {
        crate::components::chat::attachments::sent(ui, attachments);
    }
    let top = ui.cursor().top();
    let bubble_rect = (!message.is_empty()).then(|| bubble(ui, message));
    ui.set_opacity(opacity);
    if let Some(rect) = bubble_rect {
        crate::components::area_menu(ui, rect, ui.make_persistent_id("menu"), |ui| {
            crate::components::copy_item(ui, "Copy message", message);
        });
    }
    let rewind = match bubble_rect {
        Some(rect) if rewindable => rewind_button(ui, rect, top),
        _ => false,
    };
    if waiting > 0.01 {
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), 16.0 * waiting), Sense::hover());
        let color = p.text_faint.gamma_multiply(waiting);
        let label = ui.painter().layout_no_wrap(
            "Queued, runs after the current reply".to_owned(),
            font::regular(text::SM),
            color,
        );
        let right = rect.right() - space::XS;
        let y = rect.center().y;
        ui.painter().galley(
            pos2(right - label.size().x, y - label.size().y / 2.0),
            label.clone(),
            color,
        );
        glyph(
            ui,
            pos2(right - label.size().x - space::XS - icon::SM / 2.0, y),
            icons::CLOCK,
            icon::SM,
            color,
        );
    }
    rewind
}

// a small edit button left of the bubble while the message is hovered, asking before it rewinds anything
pub(super) fn rewind_button(ui: &mut Ui, bubble: Rect, top: f32) -> bool {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("rewind");
    let area = Rect::from_min_max(pos2(ui.min_rect().left(), top), bubble.max);
    let button = Rect::from_center_size(
        pos2(
            bubble.left() - space::SM - control::SM / 2.0,
            bubble.top() + 17.0,
        ),
        vec2(control::SM, control::SM),
    );
    let response = ui.interact(button, id, Sense::click());
    let menu_open = egui::Popup::is_id_open(ui.ctx(), egui::Popup::default_response_id(&response));
    let near = ui.rect_contains_pointer(area) || response.hovered() || menu_open;
    let shown = spring_bool(ui.ctx(), id.with("shown"), near, Spring::QUICK);
    if shown > 0.01 {
        let t = hover_t(ui, &response).max(if menu_open { 1.0 } else { 0.0 });
        ui.painter().rect_filled(
            button,
            CornerRadius::same(radius::MD),
            p.hover.gamma_multiply(t * shown),
        );
        glyph(
            ui,
            button.center(),
            icons::PENCIL_SIMPLE,
            icon::MD,
            p.text_faint.lerp_to_gamma(p.text, t).gamma_multiply(shown),
        );
    }
    let response = response
        .on_hover_cursor(CursorIcon::PointingHand)
        .tip("Edit from here");
    let mut rewind = false;
    egui::Popup::menu(&response)
        .align(egui::RectAlign::BOTTOM_END)
        .gap(space::XS)
        .show(|ui| {
            ui.set_width(260.0);
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                ui.add_space(space::SM);
                ui.add(
                    egui::Label::new(
                        RichText::new(
                            "This message and everything after it leave the chat, and the agent forgets them. Files stay as they are.",
                        )
                        .font(font::regular(text::SM + 1.0))
                        .color(p.text_muted),
                    )
                    .wrap(),
                );
            });
            ui.add_space(space::SM);
            if crate::components::MenuItem::new("Rewind and edit")
                .icon(icons::PENCIL_SIMPLE)
                .show(ui)
                .clicked()
            {
                rewind = true;
                ui.close();
            }
        });
    rewind
}

pub(super) fn bubble(ui: &mut Ui, message: &str) -> Rect {
    let p = theme::palette(ui);
    // lay the text out first so the bubble hugs short messages instead of filling the row
    let max = ui.available_width() * 0.8 - 24.0;
    let mut job = LayoutJob::default();
    job.wrap.max_width = max;
    // `$skill` mentions stand out, so it's clear which instructions the message pulled in
    let plain = TextFormat {
        font_id: font::regular(text::MD + 1.0),
        color: p.text,
        ..Default::default()
    };
    let skill = TextFormat {
        font_id: font::medium(text::MD + 1.0),
        color: p.accent,
        ..plain.clone()
    };
    let mut rest = message;
    while !rest.is_empty() {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let space_end = rest[word_end..]
            .find(|c: char| !c.is_whitespace())
            .map_or(rest.len(), |i| word_end + i);
        let word = &rest[..word_end];
        let mention = word
            .strip_prefix('$')
            .and_then(|name| name.chars().next())
            .is_some_and(char::is_alphabetic);
        job.append(
            word,
            0.0,
            if mention {
                skill.clone()
            } else {
                plain.clone()
            },
        );
        job.append(&rest[word_end..space_end], 0.0, plain.clone());
        rest = &rest[space_end..];
    }
    let galley = ui.painter().layout_job(job);
    let width = galley.size().x;
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        Frame::new()
            .fill(p.active)
            .corner_radius(radius::XL)
            .inner_margin(Margin::symmetric(12, 8))
            .show(ui, |ui| {
                ui.set_width(width);
                ui.add(Label::new(galley));
            })
            .response
            .rect
    })
    .inner
}
