use std::f32::consts::TAU;

use egui::{Align2, Color32, Painter, Pos2, Rect, Stroke, StrokeKind, vec2};
use mux_core::Provider;

use crate::icons;
use crate::theme::font;

// claude's signature orange, which its mark keeps even in a quiet row
pub const CLAUDE_ORANGE: Color32 = Color32::from_rgb(0xD9, 0x77, 0x57);

// draws an agent's logo: openai's from the icon font, and claude's and opencode's painted by hand since no font has them
pub fn provider_mark(
    painter: &Painter,
    center: Pos2,
    size: f32,
    provider: Provider,
    color: Color32,
) {
    match provider {
        Provider::Codex => {
            painter.text(
                center,
                Align2::CENTER_CENTER,
                icons::OPENAI_LOGO,
                font::icon(size),
                color,
            );
        }
        Provider::Claude => {
            let width = size * 0.13;
            for i in 0..12 {
                let angle = TAU * (i as f32 + 0.5) / 12.0;
                let dir = vec2(angle.cos(), angle.sin());
                // alternating ray lengths give the mark its hand drawn feel
                let reach = if i % 2 == 0 { 0.5 } else { 0.4 } * size;
                let start = center + dir * size * 0.12;
                let end = center + dir * reach;
                painter.line_segment([start, end], Stroke::new(width, color));
                painter.circle_filled(end, width / 2.0, color);
            }
        }
        // opencode's mark is a tall frame with the lower half of its inside filled in a lighter shade
        Provider::OpenCode => {
            let frame = Rect::from_center_size(center, vec2(size * 0.7, size * 0.86));
            let width = size * 0.14;
            let inside = frame.shrink(width);
            let lower =
                Rect::from_min_max(egui::pos2(inside.left(), inside.center().y), inside.max);
            painter.rect_filled(lower, 0.0, color.gamma_multiply(0.5));
            painter.rect_stroke(
                frame.shrink(width / 2.0),
                0.0,
                Stroke::new(width, color),
                StrokeKind::Middle,
            );
        }
    }
}

// the colour a mark is drawn in when nothing needs attention
pub fn provider_color(provider: Provider, quiet: Color32) -> Color32 {
    match provider {
        Provider::Codex | Provider::OpenCode => quiet,
        Provider::Claude => CLAUDE_ORANGE,
    }
}
