use egui::{
    CornerRadius, Frame, Key, KeyboardShortcut, Margin, Modifiers, Rect, RichText, Stroke, Ui,
    UiBuilder, pos2, vec2,
};

use super::ChatItem;
use crate::components::{IconButton, TooltipExt};
use crate::icons;
use crate::theme::{self, font, radius, space, text};

pub const SHORTCUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
const WIDTH: f32 = 340.0;

// words looked for in a chat, stepping through the messages that hold them
#[derive(Default)]
pub struct Find {
    open: bool,
    query: String,
    matches: Vec<usize>,
    current: usize,
    // what the matches were found for, so a long chat is searched once per change and not every frame
    searched: Option<(String, usize, usize)>,
    // frames left to keep scrolling to the match, since the blocks drawn on the way there change the heights
    settle: u8,
    focus: bool,
}

impl Find {
    // the item to show and whether the chat should still scroll to it
    pub(super) fn target(&self) -> Option<(usize, bool)> {
        let at = *self.matches.get(self.current).filter(|_| self.open)?;
        Some((at, self.settle > 0))
    }

    fn search(&mut self, items: &[ChatItem]) {
        let last = items.last().and_then(searchable).map_or(0, str::len);
        let key = (self.query.clone(), items.len(), last);
        if self.searched.as_ref() == Some(&key) {
            return;
        }
        let changed = self.searched.as_ref().is_none_or(|s| s.0 != self.query);
        self.searched = Some(key);
        let needle = self.query.trim().to_lowercase();
        self.matches = if needle.is_empty() {
            Vec::new()
        } else {
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| {
                    searchable(item).is_some_and(|t| t.to_lowercase().contains(&needle))
                })
                .map(|(i, _)| i)
                .collect()
        };
        // a new search starts from the latest message, since a chat is read from the bottom
        if changed {
            self.current = self.matches.len().saturating_sub(1);
            self.settle = 3;
        }
        self.current = self.current.min(self.matches.len().saturating_sub(1));
    }

    fn step(&mut self, by: isize) {
        let count = self.matches.len() as isize;
        if count > 0 {
            self.current = (self.current as isize + by).rem_euclid(count) as usize;
            self.settle = 3;
        }
    }
}

// what of an item can be found, which is what people and agents wrote rather than tool output
fn searchable(item: &ChatItem) -> Option<&str> {
    match item {
        ChatItem::User { text, .. }
        | ChatItem::Assistant(text)
        | ChatItem::Error(text)
        | ChatItem::Plan { text, .. } => Some(text),
        _ => None,
    }
}

// the find bar over the top right of the chat, opened with the shortcut and closed with escape
pub(super) fn bar(ui: &mut Ui, list: Rect, find: &mut Find, items: &[ChatItem]) {
    if ui.input_mut(|i| i.consume_shortcut(&SHORTCUT)) {
        find.open = true;
        find.focus = true;
    }
    if !find.open {
        return;
    }
    if ui.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
        find.open = false;
        return;
    }
    find.search(items);
    find.settle = find.settle.saturating_sub(1);
    if find.settle > 0 {
        ui.ctx().request_repaint();
    }

    let p = theme::palette(ui);
    let rect = Rect::from_min_size(
        pos2(list.right() - WIDTH - space::LG, list.top() + space::SM),
        vec2(WIDTH, 36.0),
    );
    let mut area = ui.new_child(UiBuilder::new().id_salt("find").max_rect(rect));
    Frame::new()
        .fill(p.elevated)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(radius::LG))
        .inner_margin(Margin::symmetric(space::SM as i8, 0))
        .show(&mut area, |ui| {
            ui.set_min_size(rect.size());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                if IconButton::new(icons::X, "Close find")
                    .show(ui)
                    .tip("Close")
                    .clicked()
                {
                    find.open = false;
                }
                if IconButton::new(icons::CARET_DOWN, "Next match")
                    .show(ui)
                    .tip("Next (Shift+Enter)")
                    .clicked()
                {
                    find.step(1);
                }
                if IconButton::new(icons::CARET_UP, "Previous match")
                    .show(ui)
                    .tip("Previous (Enter)")
                    .clicked()
                {
                    find.step(-1);
                }
                let count = match find.matches.len() {
                    0 if find.query.trim().is_empty() => String::new(),
                    0 => "No results".to_owned(),
                    n => format!("{} of {n}", find.current + 1),
                };
                ui.label(
                    RichText::new(count)
                        .font(font::regular(text::SM + 1.0))
                        .color(p.text_faint),
                );
                {
                    let room = ui.available_rect_before_wrap();
                    let id = ui.make_persistent_id("find-field");
                    let edit = crate::components::search_field(
                        ui,
                        room,
                        id,
                        &mut find.query,
                        "Find in chat",
                    );
                    if std::mem::take(&mut find.focus) {
                        edit.request_focus();
                    }
                    // enter goes back through older messages, since a search starts from the newest
                    // the edit's own response doesn't report focus here, but memory does
                    if ui.memory(|m| m.has_focus(edit.id)) {
                        let (back, forward) = ui.input_mut(|i| {
                            let forward = i.consume_key(Modifiers::SHIFT, Key::Enter);
                            (
                                !forward && i.consume_key(Modifiers::NONE, Key::Enter),
                                forward,
                            )
                        });
                        if back {
                            find.step(-1);
                        }
                        if forward {
                            find.step(1);
                        }
                    }
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(query: &str, items: &[ChatItem]) -> Find {
        let mut find = Find {
            open: true,
            query: query.to_owned(),
            ..Find::default()
        };
        find.search(items);
        find
    }

    #[test]
    fn finds_messages_from_the_newest_and_steps_around() {
        let items = vec![
            ChatItem::Assistant("The Parser is fine".to_owned()),
            ChatItem::Error("parser crashed".to_owned()),
            ChatItem::Assistant("nothing here".to_owned()),
            ChatItem::Assistant("a new PARSER".to_owned()),
        ];
        let mut found = find("parser", &items);
        assert_eq!(found.matches, vec![0, 1, 3]);
        assert_eq!(found.target(), Some((3, true)));
        found.step(1);
        assert_eq!(found.target().map(|t| t.0), Some(0));
        found.step(-1);
        assert_eq!(found.target().map(|t| t.0), Some(3));
    }

    #[test]
    fn nothing_is_found_for_a_blank_search_or_when_closed() {
        let items = vec![ChatItem::Assistant("text".to_owned())];
        assert!(find("  ", &items).target().is_none());
        let mut closed = find("text", &items);
        closed.open = false;
        assert!(closed.target().is_none());
    }
}
