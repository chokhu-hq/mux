use std::sync::Arc;

use egui::{Align2, Color32, CornerRadius, Galley, Pos2, Rect, Response, Sense, Ui, Vec2};
use mux_core::SessionKind;

use crate::theme::{self, Palette, font, radius, text};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Success,
    Warning,
    Danger,
    Kind(SessionKind),
}

impl Tone {
    pub(crate) fn color(self, p: &Palette) -> Color32 {
        match self {
            Self::Neutral => p.text_muted,
            Self::Accent => p.accent,
            Self::Success => p.success,
            Self::Warning => p.warning,
            Self::Danger => p.danger,
            Self::Kind(SessionKind::Agent) => p.agent,
            Self::Kind(SessionKind::Shell) => p.shell,
            Self::Kind(SessionKind::Task) => p.task,
        }
    }
}

pub struct Badge {
    text: String,
    tone: Tone,
}

impl Badge {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone: Tone::Neutral,
        }
    }

    pub fn kind(kind: SessionKind) -> Self {
        Self::new(kind.label()).tone(Tone::Kind(kind))
    }

    pub fn tone(mut self, tone: Tone) -> Self {
        self.tone = tone;
        self
    }

    pub fn show(self, ui: &mut Ui) -> Response {
        let (galley, color) = self.layout(ui);
        let (rect, response) = ui.allocate_exact_size(Self::size(&galley), Sense::hover());
        if ui.is_rect_visible(rect) {
            Self::draw(ui, rect, galley, color);
        }
        response
    }

    // drawn with its right edge at `right_center`, for rows painted by hand rather than laid out
    pub fn paint(self, ui: &Ui, right_center: Pos2) -> Rect {
        let (galley, color) = self.layout(ui);
        let rect = Align2::RIGHT_CENTER.anchor_size(right_center, Self::size(&galley));
        Self::draw(ui, rect, galley, color);
        rect
    }

    fn layout(self, ui: &Ui) -> (Arc<Galley>, Color32) {
        let color = self.tone.color(theme::palette(ui));
        let galley = ui
            .painter()
            .layout_no_wrap(self.text, font::medium(text::SM), color);
        (galley, color)
    }

    fn size(galley: &Galley) -> Vec2 {
        Vec2::new(galley.size().x + 14.0, 18.0)
    }

    fn draw(ui: &Ui, rect: Rect, galley: Arc<Galley>, color: Color32) {
        ui.painter().rect_filled(
            rect,
            CornerRadius::same(radius::FULL),
            color.gamma_multiply(0.15),
        );
        ui.painter().galley(
            Align2::CENTER_CENTER
                .anchor_size(rect.center(), galley.size())
                .min,
            galley,
            color,
        );
    }
}
