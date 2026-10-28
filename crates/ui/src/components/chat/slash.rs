use egui::text::{CCursor, CCursorRange};
use egui::{Align2, CornerRadius, Id, Key, Modifiers, Order, Pos2, Sense, Ui, vec2};

use crate::icons;
use crate::motion::{Spring, spring_bool};
use crate::theme::{self, font, icon, radius, space, text};

const ROW: f32 = 34.0;
const MAX_ROWS: usize = 7;

// a command offered when the message starts with a slash
pub struct SlashCommand {
    pub name: String,
    pub detail: String,
    // mux runs its own commands, the agent's are sent to it as the start of a message
    pub builtin: bool,
}

pub enum Pick {
    Run(String),
    // an agent's command went into the message, for its instructions to follow
    Fill,
}

// the menu of commands matching what's typed after the slash, floating just above the composer
pub fn menu(
    ui: &mut Ui,
    edit_id: Id,
    draft: &mut String,
    commands: &[SlashCommand],
    anchor: Pos2,
    width: f32,
    focused: bool,
) -> Option<Pick> {
    // a slash opening the message offers everything, a `$` word anywhere offers skills to mention
    let cursor = egui::TextEdit::load_state(ui.ctx(), edit_id)
        .and_then(|state| state.cursor.char_range())
        .map_or(draft.chars().count(), |range| range.primary.index.into());
    let at = draft
        .char_indices()
        .nth(cursor)
        .map_or(draft.len(), |(i, _)| i);
    let word_start = draft[..at].rfind(char::is_whitespace).map_or(0, |i| {
        i + draft[i..].chars().next().map_or(1, char::len_utf8)
    });
    let slash = draft.starts_with('/') && !draft.contains(char::is_whitespace);
    let mention = !slash && draft[word_start..at].starts_with('$');
    let query = if slash {
        Some(draft[1..].to_lowercase())
    } else if mention {
        Some(draft[word_start + 1..at].to_lowercase())
    } else {
        None
    };
    let commands: Vec<&SlashCommand> = commands.iter().filter(|c| slash || !c.builtin).collect();
    let state_id = edit_id.with("slash");
    // esc hides the menu for what's typed now, and it comes back once the text changes
    let dismissed = ui.data(|d| d.get_temp::<String>(state_id.with("dismissed")));
    let open = focused && query.is_some() && dismissed.as_deref() != Some(draft.as_str());
    let query = query.unwrap_or_default();
    // names that start with what's typed come first, then ones that merely contain it
    let mut matches: Vec<&SlashCommand> = commands
        .iter()
        .copied()
        .filter(|c| c.name.to_lowercase().starts_with(&query))
        .collect();
    matches.extend(
        commands
            .iter()
            .copied()
            .filter(|c| !c.name.to_lowercase().starts_with(&query))
            .filter(|c| c.name.to_lowercase().contains(&query)),
    );
    let shown = spring_bool(
        ui.ctx(),
        state_id.with("open"),
        open && !matches.is_empty(),
        Spring::QUICK,
    );
    if !open || matches.is_empty() {
        return None;
    }

    // typing more starts the choice over at the best match
    let typed_id = state_id.with("typed");
    let retyped = ui.data(|d| d.get_temp::<String>(typed_id)).as_deref() != Some(query.as_str());
    ui.data_mut(|d| d.insert_temp(typed_id, query.clone()));
    let mut selected = if retyped {
        0
    } else {
        ui.data(|d| d.get_temp::<usize>(state_id))
            .unwrap_or(0)
            .min(matches.len() - 1)
    };
    let mut chosen = None;
    let (down, up, pick, close) = ui.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Tab) || i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Escape),
        )
    });
    if down {
        selected = (selected + 1) % matches.len();
    }
    if up {
        selected = (selected + matches.len() - 1) % matches.len();
    }
    if pick {
        chosen = Some(selected);
    }
    if close {
        ui.data_mut(|d| d.insert_temp(state_id.with("dismissed"), draft.clone()));
        return None;
    }

    let p = theme::palette(ui);
    let rows = matches.len().min(MAX_ROWS);
    let height = rows as f32 * ROW + space::XS * 2.0;
    egui::Area::new(state_id.with("area"))
        .order(Order::Foreground)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(anchor - vec2(0.0, space::XS + (1.0 - shown) * 4.0))
        .show(ui.ctx(), |ui| {
            ui.multiply_opacity(shown);
            theme::floating(ui)
                .inner_margin(egui::Margin::same(space::XS as i8))
                .show(ui, |ui| {
                    ui.set_width(width - space::XS * 2.0);
                    ui.set_height(height - space::XS * 2.0);
                    egui::ScrollArea::vertical()
                        .max_height(height - space::XS * 2.0)
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (i, command) in matches.iter().enumerate() {
                                let (rect, row) = ui.allocate_exact_size(
                                    vec2(ui.available_width(), ROW),
                                    Sense::click(),
                                );
                                if row.hovered()
                                    && ui.input(|i| i.pointer.delta() != egui::Vec2::ZERO)
                                {
                                    selected = i;
                                }
                                if row.clicked() {
                                    chosen = Some(i);
                                }
                                if i == selected {
                                    ui.painter().rect_filled(
                                        rect,
                                        CornerRadius::same(radius::MD),
                                        p.accent.gamma_multiply(0.16),
                                    );
                                    if down || up {
                                        row.scroll_to_me(None);
                                    }
                                }
                                let y = rect.center().y;
                                let glyph = if command.builtin {
                                    icons::COMMAND
                                } else {
                                    icons::SPARKLE
                                };
                                ui.painter().text(
                                    egui::pos2(rect.left() + space::SM + icon::MD / 2.0, y),
                                    Align2::CENTER_CENTER,
                                    glyph,
                                    font::icon(icon::MD),
                                    p.text_muted,
                                );
                                let sigil = if command.builtin { '/' } else { '$' };
                                let name = ui.painter().text(
                                    egui::pos2(rect.left() + space::SM + icon::MD + space::SM, y),
                                    Align2::LEFT_CENTER,
                                    format!("{sigil}{}", command.name),
                                    font::medium(text::MD),
                                    p.text,
                                );
                                let room = rect.right() - space::SM - name.right() - space::MD;
                                if room > 40.0 {
                                    let mut job = egui::text::LayoutJob::simple_singleline(
                                        command.detail.clone(),
                                        font::regular(text::SM + 1.0),
                                        p.text_faint,
                                    );
                                    job.wrap = egui::text::TextWrapping {
                                        max_width: room,
                                        max_rows: 1,
                                        break_anywhere: true,
                                        overflow_character: Some('…'),
                                    };
                                    let galley = ui.painter().layout_job(job);
                                    ui.painter().galley(
                                        egui::pos2(
                                            rect.right() - space::SM - galley.size().x,
                                            y - galley.size().y / 2.0,
                                        ),
                                        galley,
                                        p.text_faint,
                                    );
                                }
                            }
                        });
                });
        });
    ui.data_mut(|d| d.insert_temp(state_id, selected));

    let command = matches.get(chosen?)?;
    if command.builtin {
        draft.clear();
        return Some(Pick::Run(command.name.clone()));
    }
    // a skill goes in as a `$name` mention wherever it was asked for, and the agent is told about it on send
    let inserted = format!("${} ", command.name);
    let (from, to) = if slash {
        (0, draft.len())
    } else {
        (word_start, at)
    };
    draft.replace_range(from..to, &inserted);
    // the cursor follows the mention, ready for whatever comes after it
    let end = draft[..from].chars().count() + inserted.chars().count();
    let mut state = egui::TextEdit::load_state(ui.ctx(), edit_id).unwrap_or_default();
    state
        .cursor
        .set_char_range(Some(CCursorRange::one(CCursor::new(end))));
    state.store(ui.ctx(), edit_id);
    Some(Pick::Fill)
}
