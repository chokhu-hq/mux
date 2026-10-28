mod badge;
mod button;
pub mod chat;
mod chip;
mod command_palette;
mod context_menu;
mod context_ring;
mod empty_state;
mod file_viewer;
mod group_header;
mod icon;
mod icon_button;
mod image_viewer;
mod kbd;
mod menu_item;
mod provider_mark;
mod segmented;
mod session_row;
mod sidebar;
mod status_dot;
mod terminal_view;
mod text_input;
mod tooltip;
mod usage_ring;

pub use badge::{Badge, Tone};
pub use button::{Button, Size, Variant};
pub use chip::Chip;
pub use command_palette::{
    Choice, Command, CommandPalette, Destination, SHORTCUT as PALETTE_SHORTCUT,
};
pub use context_menu::{area_menu, copy_item, edit_menu};
pub use context_ring::{ContextRing, tokens as short_tokens};
pub use empty_state::EmptyState;
pub use file_viewer::{FileView, FileViewAction, link_target, open_in_editor};
pub use group_header::{GroupHeader, GroupHeaderResponse};
pub use icon::Icon;
pub use icon_button::IconButton;
pub use image_viewer::ImageViewer;
pub use kbd::Kbd;
pub use menu_item::MenuItem;
pub use provider_mark::{CLAUDE_ORANGE, provider_color, provider_mark};
pub use segmented::Segmented;
pub use session_row::SessionRow;
pub use sidebar::{SHORTCUT as SIDEBAR_SHORTCUT, Sidebar};
pub use status_dot::StatusDot;
pub use terminal_view::{
    MAX_SIZE as TERMINAL_MAX_SIZE, MIN_SIZE as TERMINAL_MIN_SIZE, TerminalResponse, TerminalView,
};
pub use text_input::TextInput;
pub use tooltip::TooltipExt;
pub use usage_ring::{UsageRing, usage_rows};

use crate::motion::{Spring, spring_bool};

pub(crate) fn hover_t(ui: &egui::Ui, response: &egui::Response) -> f32 {
    spring_bool(
        ui.ctx(),
        response.id.with("hover"),
        response.hovered(),
        Spring::QUICK,
    )
}

pub(crate) fn press_t(ui: &egui::Ui, response: &egui::Response) -> f32 {
    spring_bool(
        ui.ctx(),
        response.id.with("press"),
        response.is_pointer_button_down_on(),
        Spring::SNAPPY,
    )
}

// shows `add` when open, sliding its height open and closed instead of popping in and shoving what's below
pub fn reveal(ui: &mut egui::Ui, id: egui::Id, open: bool, add: impl FnOnce(&mut egui::Ui)) {
    let t = spring_bool(ui.ctx(), id.with("reveal"), open, Spring::SMOOTH).clamp(0.0, 1.0);
    if t <= 0.001 && !open {
        return;
    }
    let width = ui.available_width();
    let top = ui.cursor().min;
    // last frame's height bounds what's visible this frame, which is exact once the content settles
    let height_id = id.with("reveal-height");
    let full = ui.data(|d| d.get_temp::<f32>(height_id)).unwrap_or(0.0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(egui::Rect::from_min_size(
        top,
        egui::vec2(width, f32::INFINITY),
    )));
    child.set_clip_rect(
        egui::Rect::from_min_size(top, egui::vec2(width, full * t)).intersect(ui.clip_rect()),
    );
    child.multiply_opacity(t);
    add(&mut child);
    let measured = child.min_rect().height();
    ui.data_mut(|d| d.insert_temp(height_id, measured));
    ui.allocate_space(egui::vec2(width, measured * t));
}
