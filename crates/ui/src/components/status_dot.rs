use egui::{Response, Sense, Ui, Vec2};
use mux_core::SessionStatus;

use crate::{motion, theme};

pub struct StatusDot {
    status: SessionStatus,
}

impl StatusDot {
    pub fn new(status: SessionStatus) -> Self {
        Self { status }
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
        if ui.is_rect_visible(rect) {
            paint(ui, rect.center(), self.status);
        }
        response.on_hover_text(self.status.label())
    }
}

// the dot alone, for places that draw it into a row of their own
pub fn paint(ui: &Ui, c: egui::Pos2, status: SessionStatus) {
    let p = theme::palette(ui);
    let color = match status {
        SessionStatus::Running => p.accent,
        SessionStatus::Waiting => p.warning,
        SessionStatus::Idle => p.text_faint,
        SessionStatus::Done => p.success,
        SessionStatus::Failed => p.danger,
    };
    if status == SessionStatus::Running && !motion::reduced(ui.ctx()) {
        let phase = (ui.input(|i| i.time) * 1.6).fract() as f32;
        ui.painter().circle_filled(
            c,
            3.5 + phase * 3.0,
            color.gamma_multiply(0.5 * (1.0 - phase)),
        );
        ui.ctx().request_repaint();
    }
    ui.painter().circle_filled(c, 3.5, color);
}
