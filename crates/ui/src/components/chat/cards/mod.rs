use std::f32::consts::{FRAC_PI_2, TAU};

use egui::epaint::TextShape;
use egui::text::LayoutJob;
use egui::{
    Align2, Color32, CornerRadius, CursorIcon, Frame, Label, Margin, Pos2, Rect, RichText,
    ScrollArea, Sense, Stroke, TextFormat, Ui, pos2, vec2,
};

use super::{
    Changes, Decision, DiffKind, FileDiff, PermissionRequest, Subagent, ToolCall, ToolKind,
    ToolStatus,
};
use crate::components::{Button, TooltipExt, hover_t, reveal};
use crate::icons;
use crate::motion::{self, Spring, spring_bool};
use crate::theme::{self, control, font, icon, radius, space, text};

mod ask;
mod diff;
mod message;
mod notice;
mod subagent;
mod tool;
mod turn;

pub use ask::permission;
pub use diff::{changes, diff};
pub use message::user_message;
pub use notice::{compaction, error, retry, switched, usage_limit};
pub use subagent::subagent;
#[cfg(test)]
use tool::one_line;
pub use tool::tool_call;
pub use turn::{activity, reasoning, summary_row, thinking, turn_summary};

pub(super) const ROW: f32 = 32.0;

// a one line preview reads markdown as plain text rather than showing its symbols
fn plain(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace("**", "")
        .replace('`', "")
}

fn card(ui: &mut Ui, add_contents: impl FnOnce(&mut Ui)) {
    theme::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        add_contents(ui);
    });
}

pub(super) fn glyph(ui: &Ui, center: Pos2, icon: char, size: f32, color: Color32) {
    ui.painter()
        .text(center, Align2::CENTER_CENTER, icon, font::icon(size), color);
}

fn rotated(ui: &Ui, center: Pos2, icon: char, size: f32, angle: f32, color: Color32) {
    let galley = ui
        .painter()
        .layout_no_wrap(icon.to_string(), font::icon(size), color);
    ui.painter().add(
        TextShape::new(center - galley.size() / 2.0, galley, color)
            .with_angle_and_anchor(angle, Align2::CENTER_CENTER),
    );
}

pub(super) fn spinner(ui: &Ui, center: Pos2, size: f32, color: Color32) {
    if motion::reduced(ui.ctx()) {
        glyph(ui, center, icons::SPINNER, size, color);
        return;
    }
    let angle = (ui.input(|i| i.time) as f32 * TAU * 0.8) % TAU;
    rotated(ui, center, icons::SPINNER, size, angle, color);
    ui.ctx().request_repaint();
}

pub(super) fn caret(ui: &Ui, center: Pos2, turn: f32, color: Color32) {
    rotated(
        ui,
        center,
        icons::CARET_RIGHT,
        12.0,
        turn * FRAC_PI_2,
        color,
    );
}
