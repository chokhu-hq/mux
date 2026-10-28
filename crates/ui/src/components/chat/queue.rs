use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Margin, Response, Sense, Stroke, StrokeKind,
    TextEdit, Ui, pos2, vec2,
};
use mux_core::protocol::{QueueAction, Queued};

use crate::components::{TooltipExt, hover_t};
use crate::icons;
use crate::theme::{self, font, icon, radius, space, text};

pub enum QueueEvent {
    Change(usize, QueueAction),
    Resume,
}

// messages waiting for the agent, at the end of the chat with an outline instead of a fill since they aren't sent yet
pub fn queue(ui: &mut Ui, messages: &[Queued], held: bool) -> Option<QueueEvent> {
    let p = theme::palette(ui);
    let editing_id = ui.make_persistent_id("queue-editing");
    let mut editing: Option<(usize, String)> = ui.data(|d| d.get_temp(editing_id));
    let mut event = None;
    if held {
        let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
        let y = row.center().y;
        let (resume, x) = action(ui, row.right(), y, icons::PLAY, Some("Resume"), None);
        if resume.tip("Send the waiting messages").clicked() {
            event = Some(QueueEvent::Resume);
        }
        ui.painter().text(
            pos2(x - space::XS, y),
            Align2::RIGHT_CENTER,
            "Waiting until you resume",
            font::regular(text::SM),
            p.text_faint,
        );
    }
    for (i, message) in messages.iter().enumerate() {
        ui.push_id(i, |ui| {
            if let Some((index, draft)) = editing.as_mut().filter(|(index, _)| *index == i) {
                // edited in place, then saved over the waiting message or left as it was
                let field = ui.add(
                    TextEdit::multiline(draft)
                        .font(font::regular(text::MD + 1.0))
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(12, 8)),
                );
                crate::components::edit_menu(&field, draft);
                // the same quiet actions as under a waiting message, with saving in the accent since it's the one that counts
                let (row, _) =
                    ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                let y = row.center().y;
                let (save, x) = action(
                    ui,
                    row.right(),
                    y,
                    icons::CHECK,
                    Some("Save"),
                    Some(p.accent),
                );
                let (cancel, _) = action(ui, x, y, icons::X, Some("Cancel"), None);
                if save.tip("Save the edit").clicked() {
                    event = Some(QueueEvent::Change(
                        *index,
                        QueueAction::Edit(draft.trim().to_owned()),
                    ));
                    editing = None;
                } else if cancel.tip("Keep it as it was").clicked() {
                    editing = None;
                }
                return;
            }
            let max = ui.available_width() * 0.8 - 24.0;
            let galley = ui.painter().layout(
                message.text.clone(),
                font::regular(text::MD + 1.0),
                p.text_muted,
                max,
            );
            let size = galley.size() + vec2(24.0, 16.0);
            let (row, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), size.y + 26.0), Sense::hover());
            let bubble = egui::Rect::from_min_size(pos2(row.right() - size.x, row.top()), size);
            ui.painter().rect_stroke(
                bubble,
                CornerRadius::same(radius::XL),
                Stroke::new(1.0, p.border_strong),
                StrokeKind::Inside,
            );
            ui.painter()
                .galley(bubble.min + vec2(12.0, 8.0), galley, p.text_muted);
            // a quiet row of what can be done with it, under the bubble
            let y = bubble.bottom() + 13.0;
            let mut x = bubble.right();
            for (glyph, label, tip, action_kind) in [
                (icons::X, "Remove", "Remove it", QueueAction::Remove),
                (
                    icons::PENCIL_SIMPLE,
                    "Edit",
                    "Edit before it's sent",
                    QueueAction::Edit(String::new()),
                ),
                (
                    icons::ARROW_UP,
                    "Send now",
                    "Send it now instead of waiting for the agent to finish",
                    QueueAction::SendNow,
                ),
            ] {
                // only sending has words, so the row stays short
                let shown = (label == "Send now").then_some(label);
                let (response, next) = action(ui, x, y, glyph, shown, None);
                x = next;
                if response.tip(tip).clicked() {
                    match action_kind {
                        QueueAction::Edit(_) => editing = Some((i, message.text.clone())),
                        action_kind => event = Some(QueueEvent::Change(i, action_kind)),
                    }
                }
            }
            ui.painter().text(
                pos2(x - space::XS, y),
                Align2::RIGHT_CENTER,
                "Queued",
                font::regular(text::SM),
                p.text_faint,
            );
        });
    }
    ui.data_mut(|d| match editing {
        Some(state) => {
            d.insert_temp(editing_id, state);
        }
        None => {
            d.remove_temp::<(usize, String)>(editing_id);
        }
    });
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
