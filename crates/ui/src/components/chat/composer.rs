use egui::{
    Align2, Color32, CursorIcon, Frame, Key, Margin, Modifiers, Response, ScrollArea, Sense,
    Stroke, TextEdit, Ui, vec2,
};

use mux_core::protocol::ContextUsage;

use crate::components::{ContextRing, IconButton, TooltipExt, hover_t, press_t};
use crate::icons;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, control, font, icon, radius, space, text};

pub enum ComposerAction {
    Send(String),
    Stop,
    Attach,
    Compact,
    // one of mux's own slash commands was picked
    Command(String),
    // up was pressed in an empty box while messages wait, to take them back and change them
    EditQueued,
}

// how full the context window gets before the composer suggests compacting
const NOTICE_AT: f32 = 0.75;

pub type Controls<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

pub struct Composer<'a> {
    draft: &'a mut String,
    running: bool,
    hint: &'a str,
    controls: Option<Controls<'a>>,
    context: Option<ContextUsage>,
    attachments: Option<&'a mut Vec<String>>,
    commands: &'a [super::SlashCommand],
    history: &'a [&'a str],
    queued: bool,
}

impl<'a> Composer<'a> {
    pub fn new(draft: &'a mut String) -> Self {
        Self {
            draft,
            running: false,
            hint: "Ask the agent…",
            controls: None,
            context: None,
            attachments: None,
            commands: &[],
            history: &[],
            queued: false,
        }
    }

    // messages sent before, oldest first, that up and down step through
    pub fn history(mut self, history: &'a [&'a str]) -> Self {
        self.history = history;
        self
    }

    // whether messages are waiting, which up takes back before reaching the history
    pub fn queued(mut self, queued: bool) -> Self {
        self.queued = queued;
        self
    }

    // what typing a slash offers
    pub fn commands(mut self, commands: &'a [super::SlashCommand]) -> Self {
        self.commands = commands;
        self
    }

    // files waiting to go with the next message, shown above the text
    pub fn attachments(mut self, attachments: &'a mut Vec<String>) -> Self {
        self.attachments = Some(attachments);
        self
    }

    // shown beside the send button once the agent has reported it
    pub fn context(mut self, context: Option<ContextUsage>) -> Self {
        self.context = context;
        self
    }

    // extra controls beside the attach button, like model and speed pickers
    pub fn controls(mut self, add: impl FnOnce(&mut Ui) + 'a) -> Self {
        self.controls = Some(Box::new(add));
        self
    }

    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }

    pub fn hint(mut self, hint: &'a str) -> Self {
        self.hint = hint;
        self
    }

    pub fn show(mut self, ui: &mut Ui) -> Option<ComposerAction> {
        let p = theme::palette(ui);
        let edit_id = ui.scope_id().with("mux-composer");
        let focused = ui.memory(|m| m.has_focus(edit_id));
        // text put in from outside, like waiting messages taken back or a rewound one, is continued from its end
        let seen_id = edit_id.with("seen");
        let seen = fingerprint(self.draft);
        if ui
            .data(|d| d.get_temp::<u64>(seen_id))
            .is_some_and(|last| last != seen)
            && !self.draft.is_empty()
            && !focused_typing(ui)
        {
            caret_to_end(ui, edit_id, self.draft);
        }
        // pasted pictures and copied files become attachments, before the text edit sees the paste as text
        if focused && let Some(attachments) = self.attachments.as_deref_mut() {
            for path in super::attachments::take_pasted(ui) {
                if !attachments.contains(&path) {
                    attachments.push(path);
                }
            }
        }
        // a message can be just the files, with nothing typed
        let has_files = self.attachments.as_ref().is_some_and(|a| !a.is_empty());
        // the slash menu reads its keys first, so enter picks a command rather than sending
        let mut picked = None;
        if !self.commands.is_empty() {
            let anchor = ui.cursor().min;
            let width = ui.available_width().min(440.0);
            if let Some(super::slash::Pick::Run(name)) = super::slash::menu(
                ui,
                edit_id,
                self.draft,
                self.commands,
                anchor,
                width,
                focused,
            ) {
                picked = Some(ComposerAction::Command(name));
            }
        }
        if focused && picked.is_none() && recall(ui, edit_id, self.draft, self.history, self.queued)
        {
            picked = Some(ComposerAction::EditQueued);
        }
        // enter sends and shift+enter falls through to the text edit as a newline
        // messages can go out while the agent works, the agent picks them up as it goes
        let mut send = focused && ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
        let mut action = picked;

        if let Some(context) = self.context
            && !self.running
            && compact_notice(ui, context)
        {
            action = Some(ComposerAction::Compact);
        }

        let focus = spring_bool(ui.ctx(), edit_id.with("focus"), focused, Spring::QUICK);
        Frame::new()
            .fill(p.elevated)
            .stroke(Stroke::new(
                1.0,
                // a faint outline like t3 code's that firms up with focus instead of turning blue
                if ui.visuals().dark_mode {
                    Color32::from_white_alpha(13)
                        .lerp_to_gamma(Color32::from_white_alpha(30), focus)
                } else {
                    Color32::from_black_alpha(20)
                        .lerp_to_gamma(Color32::from_black_alpha(40), focus)
                },
            ))
            .corner_radius(radius::XL)
            .inner_margin(Margin {
                left: 16,
                right: 12,
                top: 14,
                bottom: 12,
            })
            // only the light theme lifts it with a shadow, the dark one lets the outline do it
            .shadow(egui::Shadow {
                offset: [0, 4],
                blur: 16,
                spread: 0,
                color: Color32::from_black_alpha(if ui.visuals().dark_mode { 0 } else { 14 }),
            })
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                if let Some(attachments) = self.attachments.as_deref_mut()
                    && !attachments.is_empty()
                {
                    ui.add_space(2.0);
                    if let Some(index) = super::attachments::pending(ui, attachments) {
                        attachments.remove(index);
                    }
                    ui.add_space(space::SM);
                }
                ScrollArea::vertical()
                    .max_height(180.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        let field = ui.add(
                            TextEdit::multiline(self.draft)
                                .id(edit_id)
                                .frame(Frame::NONE)
                                .hint_text(self.hint)
                                .font(font::regular(text::MD + 1.0))
                                .desired_rows(1)
                                .desired_width(f32::INFINITY)
                                .margin(Margin::ZERO),
                        );
                        crate::components::edit_menu(&field, self.draft);
                    });
                ui.add_space(space::SM);
                ui.horizontal(|ui| {
                    if let Some(controls) = self.controls.take() {
                        // the controls get what's left once send, the context ring and attach have their room,
                        // and are clipped to it, so they shrink rather than slide under them
                        let gap = ui.spacing().item_spacing.x;
                        // room for the ring with a label as wide as 100%
                        let ring = if self.context.is_some() {
                            56.0 + gap
                        } else {
                            0.0
                        };
                        let attach = control::SM + gap;
                        let budget = (ui.available_width() - SEND - ring - attach - gap).max(0.0);
                        let row = ui.available_rect_before_wrap();
                        ui.allocate_ui_with_layout(
                            vec2(budget, control::MD),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.set_clip_rect(ui.clip_rect().intersect(
                                    egui::Rect::from_min_size(row.min, vec2(budget, row.height())),
                                ));
                                controls(ui);
                            },
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let ready = !self.draft.trim().is_empty() || has_files;
                        // while the agent works an empty box offers stop, and typing turns it back into send
                        if self.running && !ready {
                            if round_button(ui, icons::STOP, true, true)
                                .tip("Stop")
                                .clicked()
                            {
                                action = Some(ComposerAction::Stop);
                            }
                        } else {
                            let tip = if self.running {
                                "Queue it for when the agent finishes"
                            } else {
                                "Send"
                            };
                            if round_button(ui, icons::ARROW_UP, ready, false)
                                .tip(tip)
                                .clicked()
                            {
                                send = true;
                            }
                        }
                        if let Some(context) = self.context
                            && ContextRing::new(context)
                                .can_compact(!self.running)
                                .show(ui)
                        {
                            action = Some(ComposerAction::Compact);
                        }
                        if IconButton::new(icons::PAPERCLIP, "Attach")
                            .show(ui)
                            .tip("Attach files")
                            .clicked()
                        {
                            action = Some(ComposerAction::Attach);
                        }
                    });
                });
            });

        // the composer is the natural home for focus whenever nothing else holds it
        if ui.memory(|m| m.focused().is_none()) {
            ui.memory_mut(|m| m.request_focus(edit_id));
        }
        // the same command people type in the codex and claude clis
        if send && !self.running && !has_files && self.draft.trim() == "/compact" {
            self.draft.clear();
            return Some(ComposerAction::Compact);
        }
        if send && (!self.draft.trim().is_empty() || has_files) {
            let message = self.draft.trim().to_owned();
            self.draft.clear();
            action = Some(ComposerAction::Send(message));
        }
        let seen = fingerprint(self.draft);
        ui.data_mut(|d| d.insert_temp(seen_id, seen));
        action
    }
}

// shown above the composer once the context window is mostly full, so compacting is found when it matters
fn compact_notice(ui: &mut Ui, context: ContextUsage) -> bool {
    if context.window == 0 {
        return false;
    }
    let used = context.used as f32 / context.window as f32;
    if used < NOTICE_AT {
        return false;
    }
    let p = theme::palette(ui);
    let mut clicked = false;
    theme::card(ui)
        .inner_margin(Margin {
            left: 12,
            right: 4,
            top: 4,
            bottom: 4,
        })
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                ui.label(
                    egui::RichText::new(icons::ARROWS_IN_LINE_VERTICAL.to_string())
                        .font(font::icon(icon::SM))
                        .color(crate::components::usage_ring::level_color(p, used)),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "Context window {}% full",
                        (used * 100.0).round() as u32
                    ))
                    .font(font::medium(text::SM + 1.0))
                    .color(p.text),
                );
                if ui.available_width() > 360.0 {
                    ui.label(
                        egui::RichText::new("Compact to summarise older messages")
                            .font(font::regular(text::SM + 1.0))
                            .color(p.text_faint),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    clicked = crate::components::Button::new("Compact now")
                        .size(crate::components::Size::Sm)
                        .show(ui)
                        .tip("Or type /compact")
                        .clicked();
                });
            });
        });
    ui.add_space(space::SM);
    clicked
}

const SEND: f32 = 32.0;

fn round_button(ui: &mut Ui, icon: char, enabled: bool, stop: bool) -> Response {
    let p = theme::palette(ui);
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(vec2(SEND, SEND), sense);
    let t = hover_t(ui, &response);
    let pressed = press_t(ui, &response);
    let (fill, fg) = match (enabled, stop) {
        (_, true) => (p.text, p.bg),
        // filled in the text colour like t3 code, so it reads as the one thing to press
        (true, false) => (p.text.gamma_multiply(0.9).lerp_to_gamma(p.text, t), p.bg),
        (false, false) => (theme::tint(p, theme::fill::DISABLED), p.text_faint),
    };
    // grows a touch under the pointer and gives a little when pressed
    let rect = rect
        .expand(SEND * 0.025 * t * f32::from(u8::from(enabled)))
        .shrink(SEND * 0.04 * pressed);
    ui.painter()
        .circle_filled(rect.center(), rect.width() / 2.0, fill);
    let glyph = if stop {
        font::icon_fill(icon::SM)
    } else {
        font::icon(icon::MD)
    };
    ui.painter()
        .text(rect.center(), Align2::CENTER_CENTER, icon, glyph, fg);
    if enabled {
        response.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        response
    }
}

// up in an empty box takes back what's waiting, or else steps back through what was sent, and down steps forward,
// the way claude code's prompt does, returning true when the waiting messages should come back to be changed
fn recall(
    ui: &mut Ui,
    edit_id: egui::Id,
    draft: &mut String,
    history: &[&str],
    queued: bool,
) -> bool {
    let at_id = edit_id.with("recall");
    // how far back the box is showing, counted from the newest message
    let mut at: Option<usize> = ui.data(|d| d.get_temp(at_id));
    // typing over a recalled message leaves the history behind
    if at.is_some_and(|n| history.iter().rev().nth(n) != Some(&draft.as_str())) {
        at = None;
    }
    let mut taken_back = false;
    if draft.is_empty() || at.is_some() {
        let (up, down) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                at.is_some() && i.consume_key(Modifiers::NONE, Key::ArrowDown),
            )
        });
        if up {
            if queued && at.is_none() {
                taken_back = true;
            } else {
                let next = at.map_or(0, |n| n + 1);
                if let Some(text) = history.iter().rev().nth(next) {
                    *draft = (*text).to_owned();
                    at = Some(next);
                    caret_to_end(ui, edit_id, draft);
                }
            }
        } else if down && let Some(n) = at {
            if n == 0 {
                draft.clear();
                at = None;
            } else if let Some(text) = history.iter().rev().nth(n - 1) {
                *draft = (*text).to_owned();
                at = Some(n - 1);
                caret_to_end(ui, edit_id, draft);
            }
        }
    }
    ui.data_mut(|d| match at {
        Some(n) => {
            d.insert_temp(at_id, n);
        }
        None => {
            d.remove_temp::<usize>(at_id);
        }
    });
    taken_back
}

fn caret_to_end(ui: &Ui, edit_id: egui::Id, text: &str) {
    let mut state = TextEdit::load_state(ui.ctx(), edit_id).unwrap_or_default();
    let end = egui::text::CCursor::new(text.chars().count());
    state
        .cursor
        .set_char_range(Some(egui::text::CCursorRange::one(end)));
    state.store(ui.ctx(), edit_id);
}

// keys or text arriving this frame mean the change is the person typing, not text put in from outside
fn focused_typing(ui: &Ui) -> bool {
    ui.input(|i| {
        i.events.iter().any(|e| {
            matches!(
                e,
                egui::Event::Text(_) | egui::Event::Paste(_) | egui::Event::Key { .. }
            )
        })
    })
}

fn fingerprint(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::hash::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}
