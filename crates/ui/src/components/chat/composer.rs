use egui::{
    Align2, Color32, CursorIcon, Frame, Key, Margin, Modifiers, Response, ScrollArea, Sense,
    Stroke, TextEdit, Ui, vec2,
};

use crate::components::{IconButton, TooltipExt, hover_t, press_t};
use crate::icons;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, font, radius, space, text};

pub enum ComposerAction {
    Send(String),
    Stop,
}

pub struct Composer<'a> {
    draft: &'a mut String,
    running: bool,
    hint: &'a str,
}

impl<'a> Composer<'a> {
    pub fn new(draft: &'a mut String) -> Self {
        Self {
            draft,
            running: false,
            hint: "Ask the agent…",
        }
    }

    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Option<ComposerAction> {
        let p = theme::palette(ui);
        let edit_id = ui.scope_id().with("mux-composer");
        let focused = ui.memory(|m| m.has_focus(edit_id));
        // enter sends and shift+enter falls through to the text edit as a newline
        let mut send = focused
            && !self.running
            && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let mut action = None;

        let focus = spring_bool(ui.ctx(), edit_id.with("focus"), focused, Spring::QUICK);
        Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(
                1.0,
                p.border_strong
                    .lerp_to_gamma(p.accent.gamma_multiply(0.7), focus),
            ))
            .corner_radius(radius::LG + 4)
            .inner_margin(Margin {
                left: 12,
                right: 8,
                top: 10,
                bottom: 8,
            })
            .shadow(egui::Shadow {
                offset: [0, 4],
                blur: 16,
                spread: 0,
                color: Color32::from_black_alpha(if ui.visuals().dark_mode { 60 } else { 12 }),
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ScrollArea::vertical()
                    .max_height(180.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        ui.add(
                            TextEdit::multiline(self.draft)
                                .id(edit_id)
                                .frame(Frame::NONE)
                                .hint_text(self.hint)
                                .font(font::regular(text::MD))
                                .desired_rows(1)
                                .desired_width(f32::INFINITY)
                                .margin(Margin::ZERO),
                        );
                    });
                ui.add_space(space::XS);
                ui.horizontal(|ui| {
                    IconButton::new(icons::PAPERCLIP, "Attach")
                        .show(ui)
                        .tip("Attach files");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.running {
                            if round_button(ui, icons::STOP, true, true)
                                .tip("Stop")
                                .clicked()
                            {
                                action = Some(ComposerAction::Stop);
                            }
                        } else {
                            let ready = !self.draft.trim().is_empty();
                            if round_button(ui, icons::ARROW_UP, ready, false)
                                .tip("Send")
                                .clicked()
                            {
                                send = true;
                            }
                        }
                    });
                });
            });

        // the composer is the natural home for focus whenever nothing else holds it
        if ui.memory(|m| m.focused().is_none()) {
            ui.memory_mut(|m| m.request_focus(edit_id));
        }
        if send && !self.draft.trim().is_empty() {
            let message = self.draft.trim().to_owned();
            self.draft.clear();
            action = Some(ComposerAction::Send(message));
        }
        action
    }
}

fn round_button(ui: &mut Ui, icon: char, enabled: bool, stop: bool) -> Response {
    let p = theme::palette(ui);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(26.0, 26.0), sense);
    let t = hover_t(ui, &response);
    let pressed = press_t(ui, &response);
    let (fill, fg) = match (enabled, stop) {
        (_, true) => (p.text, p.bg),
        (true, false) => (p.accent.lerp_to_gamma(p.accent_hover, t), p.on_accent),
        (false, false) => (p.active, p.text_faint),
    };
    let rect = rect.shrink(26.0 * 0.04 * pressed);
    ui.painter()
        .circle_filled(rect.center(), rect.width() / 2.0, fill);
    let glyph = if stop {
        font::icon_fill(11.0)
    } else {
        font::icon(14.0)
    };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, icon, glyph, fg);
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}
