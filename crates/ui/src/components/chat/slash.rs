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

// how many files the `@` menu offers at once, so a big repo stays quick to filter
const MAX_FILES: usize = 50;

// one line of the menu, a command to run or a skill or file to mention
struct Row<'a> {
    label: String,
    detail: &'a str,
    glyph: char,
    // mux's own command to run, otherwise the text put in place of the typed word
    run: Option<&'a str>,
    insert: String,
}

// the menu of commands matching what's typed after the slash, floating just above the composer
#[allow(clippy::too_many_arguments)]
pub fn menu(
    ui: &mut Ui,
    edit_id: Id,
    draft: &mut String,
    commands: &[SlashCommand],
    files: &[String],
    anchor: Pos2,
    width: f32,
    focused: bool,
) -> Option<Pick> {
    // a slash opening the message offers everything, a `$` word anywhere offers skills and an `@` word files
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
    let file = !slash && draft[word_start..at].starts_with('@');
    let query = if slash {
        Some(draft[1..].to_lowercase())
    } else if mention || file {
        Some(draft[word_start + 1..at].to_lowercase())
    } else {
        None
    };
    let state_id = edit_id.with("slash");
    // esc hides the menu for what's typed now, and it comes back once the text changes
    let dismissed = ui.data(|d| d.get_temp::<String>(state_id.with("dismissed")));
    let open = focused && query.is_some() && dismissed.as_deref() != Some(draft.as_str());
    let query = query.unwrap_or_default();
    let matches = if file {
        file_rows(files, &query)
    } else {
        command_rows(commands, slash, &query)
    };
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
                            for (i, row_data) in matches.iter().enumerate() {
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
                                ui.painter().text(
                                    egui::pos2(rect.left() + space::SM + icon::MD / 2.0, y),
                                    Align2::CENTER_CENTER,
                                    row_data.glyph,
                                    font::icon(icon::MD),
                                    p.text_muted,
                                );
                                let name = ui.painter().text(
                                    egui::pos2(rect.left() + space::SM + icon::MD + space::SM, y),
                                    Align2::LEFT_CENTER,
                                    &row_data.label,
                                    font::medium(text::MD),
                                    p.text,
                                );
                                let room = rect.right() - space::SM - name.right() - space::MD;
                                if room > 40.0 {
                                    let mut job = egui::text::LayoutJob::simple_singleline(
                                        row_data.detail.to_owned(),
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

    let row = matches.get(chosen?)?;
    if let Some(name) = row.run {
        draft.clear();
        return Some(Pick::Run(name.to_owned()));
    }
    // a skill goes in as a `$name` mention and a file as `@path`, wherever it was asked for
    let inserted = format!("{} ", row.insert);
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

// names that start with what's typed come first, then ones that merely contain it
fn command_rows<'a>(commands: &'a [SlashCommand], slash: bool, query: &str) -> Vec<Row<'a>> {
    let commands: Vec<&SlashCommand> = commands.iter().filter(|c| slash || !c.builtin).collect();
    let starts = |c: &&&SlashCommand| c.name.to_lowercase().starts_with(query);
    let first = commands.iter().filter(starts);
    let rest = commands
        .iter()
        .filter(|c| !starts(c) && c.name.to_lowercase().contains(query));
    first
        .chain(rest)
        .map(|c| {
            let sigil = if c.builtin { '/' } else { '$' };
            Row {
                label: format!("{sigil}{}", c.name),
                detail: &c.detail,
                glyph: if c.builtin {
                    icons::COMMAND
                } else {
                    icons::SPARKLE
                },
                run: c.builtin.then_some(c.name.as_str()),
                insert: format!("${}", c.name),
            }
        })
        .collect()
}

// files whose name starts with what's typed come first, then any whose path contains it, shallower paths before deeper
fn file_rows<'a>(files: &'a [String], query: &str) -> Vec<Row<'a>> {
    let mut found: Vec<(bool, usize, &'a str)> = files
        .iter()
        .filter_map(|path| {
            let lower = path.to_lowercase();
            if file_name(&lower).starts_with(query) {
                Some((false, path.matches('/').count(), path.as_str()))
            } else if lower.contains(query) {
                Some((true, path.matches('/').count(), path.as_str()))
            } else {
                None
            }
        })
        .collect();
    found.sort();
    found
        .into_iter()
        .take(MAX_FILES)
        .map(|(_, _, path)| {
            let file = file_name(path);
            Row {
                label: file.to_owned(),
                detail: path[..path.len() - file.len()].trim_end_matches('/'),
                glyph: icons::FILE,
                run: None,
                // claude code's way of mentioning a path with spaces in it
                insert: if path.contains(char::is_whitespace) {
                    format!("@\"{path}\"")
                } else {
                    format!("@{path}")
                },
            }
        })
        .collect()
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}
