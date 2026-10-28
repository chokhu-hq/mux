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
        let p = theme::palette(ui);
        let color = match self.status {
            SessionStatus::Running => p.accent,
            SessionStatus::Waiting => p.warning,
            SessionStatus::Idle => p.text_faint,
            SessionStatus::Done => p.success,
            SessionStatus::Failed => p.danger,
        };
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
        if ui.is_rect_visible(rect) {
            let c = rect.center();
            if self.status == SessionStatus::Running && !motion::reduced(ui.ctx()) {
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
        response.on_hover_text(self.status.label())
    }
}
