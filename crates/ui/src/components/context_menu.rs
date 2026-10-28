use egui::text::{CCursor, CCursorRange};
use egui::{Id, Popup, Rect, Response, Sense, SetOpenCommand, TextEdit, Ui, ViewportCommand};

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
    let response = ui.interact(rect, id, Sense::hover());
    let opened = response.contains_pointer() && ui.input(|i| i.pointer.secondary_clicked());
    Popup::menu(&response)
        .open_memory(opened.then_some(SetOpenCommand::Bool(true)))
        .at_pointer_fixed()
        .show(|ui| {
            ui.set_min_width(180.0);
            add(ui);
        });
}

// one row that copies a piece of text and closes the menu
pub fn copy_item(ui: &mut Ui, label: &str, text: &str) {
    if MenuItem::new(label).icon(icons::COPY).show(ui).clicked() {
        ui.ctx().copy_text(text.to_owned());
        ui.close();
    }
}
