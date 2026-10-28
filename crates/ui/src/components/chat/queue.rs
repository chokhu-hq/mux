use egui::{
    Align2, CornerRadius, CursorIcon, Margin, RichText, Sense, Stroke, StrokeKind, TextEdit, Ui,
    pos2, vec2,
};
use mux_core::protocol::{QueueAction, Queued};

use crate::components::{Button, Size, TooltipExt, hover_t};
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
        ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if Button::new("Resume")
                    .size(Size::Sm)
                    .show(ui)
                    .tip("Send the waiting messages")
                    .clicked()
                {
                    event = Some(QueueEvent::Resume);
                }
                ui.label(
                    RichText::new("Waiting until you resume")
                        .font(font::regular(text::SM + 1.0))
                        .color(p.text_faint),
                );
            });
        });
    }
    for (i, message) in messages.iter().enumerate() {
        ui.push_id(i, |ui| {
            if let Some((index, draft)) = editing.as_mut().filter(|(index, _)| *index == i) {
                // edited in place, then saved over the waiting message or left as it was
                ui.add(
                    TextEdit::multiline(draft)
                        .font(font::regular(text::MD + 1.0))
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .margin(Margin::symmetric(12, 8)),
                );
                ui.add_space(space::XS);
                let mut done = false;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if Button::new("Save")
                        .primary()
                        .size(Size::Sm)
                        .show(ui)
                        .clicked()
                    {
                        event = Some(QueueEvent::Change(
                            *index,
                            QueueAction::Edit(draft.trim().to_owned()),
                        ));
                        done = true;
                    }
                    if Button::new("Cancel").size(Size::Sm).show(ui).clicked() {
                        done = true;
                    }
                });
                if done {
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
            for (glyph, label, action) in [
                (icons::X, "Remove", QueueAction::Remove),
                (
                    icons::PENCIL_SIMPLE,
                    "Edit",
                    QueueAction::Edit(String::new()),
                ),
                (icons::ARROW_UP, "Send now", QueueAction::SendNow),
            ] {
                let width = if label == "Send now" { 74.0 } else { 22.0 };
                let rect = egui::Rect::from_min_max(pos2(x - width, y - 10.0), pos2(x, y + 10.0));
                x -= width + space::XS;
                let response = ui.interact(rect, ui.scope_id().with(label), Sense::click());
                let t = hover_t(ui, &response);
                ui.painter().rect_filled(
                    rect,
                    CornerRadius::same(radius::MD),
                    p.hover.gamma_multiply(t),
                );
                let color = p.text_faint.lerp_to_gamma(p.text, t);
                if label == "Send now" {
                    ui.painter().text(
                        pos2(rect.left() + 6.0 + icon::SM / 2.0, y),
                        Align2::CENTER_CENTER,
                        glyph,
                        font::icon(icon::SM),
                        color,
                    );
                    ui.painter().text(
                        pos2(rect.left() + 6.0 + icon::SM + space::XS, y),
                        Align2::LEFT_CENTER,
                        label,
                        font::regular(text::SM + 1.0),
                        color,
                    );
                } else {
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        glyph,
                        font::icon(icon::SM),
                        color,
                    );
                }
                let tip = match label {
                    "Send now" => "Send it now instead of waiting for the agent to finish",
                    "Edit" => "Edit before it's sent",
                    _ => "Remove it",
                };
                if response
                    .on_hover_cursor(CursorIcon::PointingHand)
                    .tip(tip)
                    .clicked()
                {
                    match action {
                        QueueAction::Edit(_) => editing = Some((i, message.text.clone())),
                        action => event = Some(QueueEvent::Change(i, action)),
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
