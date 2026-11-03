use egui::{CursorIcon, Id, Key, KeyboardShortcut, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::motion::{Spring, spring_bool};
use crate::shortcut;
use crate::theme;

pub const SHORTCUT: KeyboardShortcut = shortcut::app(Key::B);
const MIN_WIDTH: f32 = 200.0;
const MAX_WIDTH: f32 = 420.0;
const HANDLE: f32 = 6.0;

// how much width the chat keeps beside the sidebar before the sidebar tucks itself away
const ROOM: f32 = 600.0;

pub struct Sidebar {
    open: bool,
    // closed because the window got narrow, so it comes back once there's room again
    tucked: bool,
    narrow: bool,
    width: f32,
    // how far open it is this frame, between 0 and 1
    t: f32,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            open: true,
            tucked: false,
            narrow: false,
            width: 260.0,
            t: 1.0,
        }
    }
}

impl Sidebar {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.tucked = false;
    }

    // tucks the sidebar away as the window gets too narrow for the chat beside it, and brings it back once
    // it widens again. only crossing the line acts, so opening or closing it by hand while narrow still holds
    pub fn fit(&mut self, window_width: f32) {
        let narrow = window_width < self.width + ROOM;
        if narrow == self.narrow {
            return;
        }
        self.narrow = narrow;
        if narrow && self.open {
            self.open = false;
            self.tucked = true;
        } else if !narrow && self.tucked {
            self.open = true;
            self.tucked = false;
        }
    }

    // advances the open animation and returns the visible width, so the window can paint behind it first
    pub fn animate(&mut self, ctx: &egui::Context) -> f32 {
        let id = Id::unique("mux-sidebar");
        self.t = spring_bool(ctx, id.with("open"), self.open, Spring::SMOOTH).max(0.0);
        self.width * self.t
    }

    pub fn show(&mut self, ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) -> Rect {
        let p = theme::palette(ui);
        let id = Id::unique("mux-sidebar");
        let rect = ui.max_rect();
        let t = self.t;
        let edge = rect.min.x + self.width * t;
        let visible = Rect::from_min_max(rect.min, pos2(edge, rect.max.y));

        if edge > rect.min.x + 0.5 {
            // slide the full-width contents in instead of squeezing them while animating
            let inner = Rect::from_min_size(
                pos2(edge - self.width, rect.min.y),
                vec2(self.width, rect.height()),
            );
            let mut child = ui.new_child(UiBuilder::new().max_rect(inner));
            child.set_clip_rect(visible.intersect(ui.clip_rect()));
            child.multiply_opacity(t.min(1.0));
            add_contents(&mut child);
        }

        if self.open {
            let handle =
                Rect::from_center_size(pos2(edge, rect.center().y), vec2(HANDLE, rect.height()));
            let response = ui
                .interact(handle, id.with("resize"), Sense::drag())
                .on_hover_cursor(CursorIcon::ResizeHorizontal);
            if response.dragged()
                && let Some(pointer) = response.interact_pointer_pos()
            {
                self.width = (pointer.x - rect.min.x).clamp(MIN_WIDTH, MAX_WIDTH);
            }
            if response.hovered() || response.dragged() {
                ui.painter().vline(
                    edge - 0.5,
                    rect.y_range(),
                    Stroke::new(1.0, p.border_strong),
                );
            }
        }

        Rect::from_min_max(pos2(edge, rect.min.y), rect.max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tucks_away_in_a_narrow_window_and_comes_back_when_it_widens() {
        let mut sidebar = Sidebar::default();
        sidebar.fit(1200.0);
        assert!(sidebar.is_open());
        sidebar.fit(800.0);
        assert!(!sidebar.is_open(), "too narrow for the chat beside it");
        sidebar.fit(1200.0);
        assert!(sidebar.is_open(), "back once there's room");

        // opened by hand while narrow, it stays open
        sidebar.fit(800.0);
        sidebar.toggle();
        sidebar.fit(790.0);
        assert!(sidebar.is_open());
        // and closed by hand, it stays closed when the window widens
        sidebar.toggle();
        sidebar.fit(1200.0);
        assert!(!sidebar.is_open());
    }
}
