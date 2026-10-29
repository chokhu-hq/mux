use egui::text::{CCursor, CCursorRange};
use egui::text_selection::LabelSelectionState;
use egui::{
    Context, Event, FullOutput, Id, OutputCommand, PointerButton, Popup, Pos2, RawInput, Rect,
    Response, SetOpenCommand, TextEdit, Ui, ViewportCommand,
};

use super::MenuItem;
use crate::icons;

// cut, copy, paste and select all on right click, the way text fields everywhere else offer them
pub fn edit_menu(response: &Response, text: &mut String) {
    let id = response.id;
    Popup::context_menu(response).show(|ui| {
        ui.set_min_width(180.0);
        let ctx = ui.ctx().clone();
        let mut state = TextEdit::load_state(&ctx, id).unwrap_or_default();
        let chars = text.chars().count();
        let selected = state
            .cursor
            .char_range()
            .map(|r| {
                let (a, b) = (r.primary.index.0, r.secondary.index.0);
                (a.min(b).min(chars), a.max(b).min(chars))
            })
            .filter(|(a, b)| a < b);
        let byte = |index: usize| {
            text.char_indices()
                .nth(index)
                .map_or(text.len(), |(at, _)| at)
        };
        if let Some((from, to)) = selected {
            let (start, end) = (byte(from), byte(to));
            if MenuItem::new("Cut").show(ui).clicked() {
                ctx.copy_text(text[start..end].to_owned());
                text.replace_range(start..end, "");
                state
                    .cursor
                    .set_char_range(Some(CCursorRange::one(CCursor::new(from))));
                state.clone().store(&ctx, id);
                ctx.memory_mut(|m| m.request_focus(id));
                ui.close();
            }
            if MenuItem::new("Copy").show(ui).clicked() {
                ctx.copy_text(text[start..end].to_owned());
                ui.close();
            }
        }
        // the field takes the keyboard back first, so the pasted text lands in it
        if MenuItem::new("Paste").show(ui).clicked() {
            ctx.memory_mut(|m| m.request_focus(id));
            ctx.send_viewport_cmd(ViewportCommand::RequestPaste);
            ui.close();
        }
        if chars > 0 && MenuItem::new("Select all").show(ui).clicked() {
            state.cursor.set_char_range(Some(CCursorRange::two(
                CCursor::new(0),
                CCursor::new(chars),
            )));
            state.store(&ctx, id);
            ctx.memory_mut(|m| m.request_focus(id));
            ui.close();
        }
    });
}

// a right-click menu for an area whose own contents take the clicks, like a message made of selectable text,
// so it opens wherever the pointer is inside the area without stealing clicks from what's in it
pub fn area_menu(ui: &Ui, rect: Rect, id: Id, add: impl FnOnce(&mut Ui)) {
    // nothing is registered over the area, since even a hover-only widget there would take the pointer from
    // the text beneath it, and a press away from the text clears whatever was selected in it
    let ctx = ui.ctx();
    let clicked = ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.secondary_clicked());
    // a right-click held back to keep a selection alive counts as one on this area
    let held = ctx
        .data(|d| d.get_temp::<Pos2>(held_click_id()))
        .filter(|pos| rect.contains(*pos));
    if held.is_some() {
        ctx.data_mut(|d| d.remove_temp::<Pos2>(held_click_id()));
    }
    let opened = clicked || held.is_some();
    // what was selected when the menu opened, since pressing on the menu clears the selection before the click lands
    let picked = id.with("picked");
    if opened {
        let selected = selected_text(ctx);
        ctx.data_mut(|d| {
            if let Some(text) = selected {
                d.insert_temp(picked, text);
            } else {
                d.remove_temp::<String>(picked);
            }
        });
    }
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<Rect>>(menu_areas_id())
            .push(rect)
    });
    Popup::new(
        id,
        ui.ctx().clone(),
        egui::PopupAnchor::PointerFixed,
        ui.layer_id(),
    )
    .kind(egui::PopupKind::Menu)
    .layout(egui::Layout::top_down_justified(egui::Align::Min))
    .style(egui::containers::menu::menu_style)
    .gap(0.0)
    .open_memory(opened.then_some(SetOpenCommand::Bool(true)))
    .show(|ui| {
        ui.set_min_width(180.0);
        // text picked out of a message comes first, since that's usually what a right-click there is for
        if let Some(selected) = ui.ctx().data(|d| d.get_temp::<String>(picked)) {
            // the shortcut shows it does what pressing it would, and the message's own copy follows straight on
            let shortcut = ui.ctx().format_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::C,
            ));
            if MenuItem::new("Copy selection")
                .icon(icons::COPY)
                .shortcut(&shortcut)
                .show(ui)
                .clicked()
            {
                ui.ctx().copy_text(selected);
                ui.close();
            }
        }
        add(ui);
    });
}

// keeps the last text selected across labels, which egui reports once a selection settles
pub(crate) struct SelectionWatch;

impl egui::Plugin for SelectionWatch {
    fn debug_name(&self) -> &'static str {
        "mux selection watch"
    }

    // egui starts a new selection on any button pressed over text, right included, which would clear what was
    // selected before its menu opens, so a right-click on a message with text selected is kept from egui
    // and handed to that message's menu instead
    fn input_hook(&mut self, ctx: &Context, input: &mut RawInput) {
        let areas = ctx
            .data_mut(|d| std::mem::take(d.get_temp_mut_or_default::<Vec<Rect>>(menu_areas_id())));
        if selected_text(ctx).is_none() {
            return;
        }
        input.events.retain(|event| match event {
            Event::PointerButton {
                button: PointerButton::Secondary,
                pressed,
                pos,
                ..
            } if areas.iter().any(|area| area.contains(*pos)) => {
                if !pressed {
                    ctx.data_mut(|d| d.insert_temp(held_click_id(), *pos));
                }
                false
            }
            _ => true,
        });
    }

    fn output_hook(&mut self, ctx: &Context, output: &mut FullOutput) {
        for command in &output.platform_output.commands {
            if let OutputCommand::TextSelectionSettled(text) = command {
                ctx.data_mut(|d| d.insert_temp(selection_id(), text.clone()));
            }
        }
    }
}

fn selection_id() -> Id {
    Id::unique("mux-selected-text")
}

// the message areas with a menu, as last drawn, so a right-click can be matched to one before egui sees it
fn menu_areas_id() -> Id {
    Id::unique("mux-menu-areas")
}

fn held_click_id() -> Id {
    Id::unique("mux-held-right-click")
}

// the text selected in chat right now, if anything is
pub fn selected_text(ctx: &Context) -> Option<String> {
    if !ctx.plugin::<LabelSelectionState>().lock().has_selection() {
        return None;
    }
    ctx.data(|d| d.get_temp::<String>(selection_id()))
        .filter(|text| !text.trim().is_empty())
}

// one row that copies a piece of text and closes the menu
pub fn copy_item(ui: &mut Ui, label: &str, text: &str) {
    if MenuItem::new(label).icon(icons::COPY).show(ui).clicked() {
        ui.ctx().copy_text(text.to_owned());
        ui.close();
    }
}
