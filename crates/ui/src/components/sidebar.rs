use egui::{CursorIcon, Id, Key, KeyboardShortcut, Rect, Sense, Stroke, Ui, UiBuilder, pos2, vec2};

use crate::motion::{Spring, spring_bool};
use crate::shortcut;
use crate::theme;

pub const SHORTCUT: KeyboardShortcut = shortcut::app(Key::B);
const MIN_WIDTH: f32 = 200.0;
const MAX_WIDTH: f32 = 420.0;
const HANDLE: f32 = 6.0;

pub struct Sidebar {
    open: bool,
    width: f32,
}

impl Default for Sidebar {
    fn default() -> Self {
        Self {
            open: true,
            width: 260.0,
        }
    }
}

impl Sidebar {
    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn show(&mut self, ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) -> Rect {
        let p = theme::palette(ui);
        let id = Id::unique("mux-sidebar");
        let rect = ui.max_rect();
        let t = spring_bool(ui.ctx(), id.with("open"), self.open, Spring::SMOOTH).max(0.0);
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
            ui.painter()
                .vline(edge - 0.5, rect.y_range(), Stroke::new(1.0, p.border));
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
