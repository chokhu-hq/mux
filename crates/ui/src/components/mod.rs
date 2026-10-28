mod badge;
mod button;
pub mod chat;
mod command_palette;
mod empty_state;
mod group_header;
mod icon;
mod icon_button;
mod kbd;
mod menu_item;
mod segmented;
mod session_row;
mod sidebar;
mod status_dot;
mod terminal_view;
mod text_input;
mod tooltip;

pub use badge::{Badge, Tone};
pub use button::{Button, Size, Variant};
pub use command_palette::{Command, CommandPalette, SHORTCUT as PALETTE_SHORTCUT};
pub use empty_state::EmptyState;
pub use group_header::GroupHeader;
pub use icon::Icon;
pub use icon_button::IconButton;
pub use kbd::Kbd;
pub use menu_item::MenuItem;
pub use segmented::Segmented;
pub use session_row::SessionRow;
pub use sidebar::{SHORTCUT as SIDEBAR_SHORTCUT, Sidebar};
pub use status_dot::StatusDot;
pub use terminal_view::{TerminalResponse, TerminalView};
pub use text_input::TextInput;
pub use tooltip::TooltipExt;

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
