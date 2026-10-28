use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Frame, Margin, Response, Sense, Stroke, Ui, pos2,
    vec2,
};
use mux_core::protocol::Queued;

use super::cards::{ROW, glyph};
use crate::components::{TooltipExt, hover_t};
use crate::icons;
use crate::theme::{self, font, icon, radius, space, text};

pub enum QueueEvent {
    SendNow(usize),
    Remove(usize),
    // back into the composer to change, like pressing up
    Edit(usize),
    Resume,
}

// messages waiting for the agent, just above the composer the way claude code shows them,
// since the agent takes them in at its next step rather than where they'd sit in the chat
pub fn strip(ui: &mut Ui, messages: &[Queued], held: bool) -> Option<QueueEvent> {
    if messages.is_empty() {
        return None;
    }
    let p = theme::palette(ui);
    let mut event = None;
    Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(radius::LG)
        .inner_margin(Margin::same(space::XS as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, message) in messages.iter().enumerate() {
                ui.push_id(i, |ui| {
                    if let Some(e) = row(ui, i, message) {
                        event = Some(e);
                    }
                });
            }
            // what happens next, and why nothing has gone yet when a stop held the queue
            let (line, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 24.0), Sense::hover());
            let y = line.center().y;
            if held {
                let (resume, _) = action(
                    ui,
                    line.right() - space::XS,
                    y,
                    icons::PLAY,
                    Some("Resume"),
                    None,
                );
                if resume.tip("Send the waiting messages").clicked() {
                    event = Some(QueueEvent::Resume);
                }
            }
            let note = if held {
                "Waiting until you resume · ↑ to edit"
            } else {
                "Goes to the agent at its next step · ↑ to edit"
            };
            ui.painter().text(
                pos2(line.left() + space::SM, y),
                Align2::LEFT_CENTER,
                note,
                font::regular(text::SM),
                p.text_faint,
            );
        });
    ui.add_space(space::SM);
    event
}

// one waiting message on a line, with what can be done to it
fn row(ui: &mut Ui, index: usize, message: &Queued) -> Option<QueueEvent> {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    let t = hover_t(ui, &response);
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius::MD),
        p.hover.gamma_multiply(t),
    );
    let y = rect.center().y;
    let mut event = None;
    let mut x = rect.right() - space::XS;
    for (glyph_char, label, tip, made) in [
        (
            icons::X,
            None,
            "Remove it",
            QueueEvent::Remove as fn(usize) -> QueueEvent,
        ),
        (
            icons::PENCIL_SIMPLE,
            None,
            "Edit it in the box below",
            QueueEvent::Edit,
        ),
        (
            icons::ARROW_UP,
            Some("Send now"),
            "Hand it to the agent now instead of at its next step",
            QueueEvent::SendNow,
        ),
    ] {
        let (clicked, next) = action(ui, x, y, glyph_char, label, None);
        x = next;
        if clicked.tip(tip).clicked() {
            event = Some(made(index));
        }
    }
    let left = rect.left() + space::SM;
    glyph(
        ui,
        pos2(left + icon::MD / 2.0, y),
        icons::CLOCK,
        icon::MD,
        p.text_faint,
    );
    let text_left = left + icon::MD + space::SM;
    let mut shown = message
        .text
        .lines()
        .map(str::trim)
        .collect::<Vec<_>>()
        .join(" ");
    if !message.attachments.is_empty() {
        shown = format!("{shown} · {} attached", message.attachments.len());
    }
    let mut job =
        egui::text::LayoutJob::simple_singleline(shown, font::regular(text::MD), p.text_muted);
    job.wrap = egui::text::TextWrapping::truncate_at_width((x - space::SM - text_left).max(0.0));
    let galley = ui.painter().layout_job(job);
    ui.painter().galley(
        pos2(text_left, y - galley.size().y / 2.0),
        galley,
        p.text_muted,
    );
    event
}

// one quiet action, an icon with or without words, laid right to left from `right`,
// returning its response and where the next one to its left ends
fn action(
    ui: &mut Ui,
    right: f32,
    y: f32,
    glyph: char,
    label: Option<&str>,
    color: Option<Color32>,
) -> (Response, f32) {
    let p = theme::palette(ui);
    let words = label.map(|label| {
        ui.painter().layout_no_wrap(
            label.to_owned(),
            font::regular(text::SM + 1.0),
            Color32::PLACEHOLDER,
        )
    });
    let width = match &words {
        Some(words) => 6.0 + icon::SM + space::XS + words.size().x + 8.0,
        None => 22.0,
    };
    let rect = egui::Rect::from_min_max(pos2(right - width, y - 10.0), pos2(right, y + 10.0));
    let id = ui.scope_id().with(("queue-action", glyph));
    let response = ui.interact(rect, id, Sense::click());
    let t = hover_t(ui, &response);
    ui.painter().rect_filled(
        rect,
        CornerRadius::same(radius::MD),
        p.hover.gamma_multiply(t),
    );
    let rest = color.unwrap_or(p.text_faint);
    let tint = rest.lerp_to_gamma(color.unwrap_or(p.text), t);
    match words {
        Some(words) => {
            ui.painter().text(
                pos2(rect.left() + 6.0 + icon::SM / 2.0, y),
                Align2::CENTER_CENTER,
                glyph,
                font::icon(icon::SM),
                tint,
            );
            ui.painter().galley(
                pos2(
                    rect.left() + 6.0 + icon::SM + space::XS,
                    y - words.size().y / 2.0,
                ),
                words,
                tint,
            );
        }
        None => {
            ui.painter().text(
                rect.center(),
                Align2::CENTER_CENTER,
                glyph,
                font::icon(icon::SM),
                tint,
            );
        }
    }
    (
        response.on_hover_cursor(CursorIcon::PointingHand),
        rect.left() - space::XS,
    )
}
