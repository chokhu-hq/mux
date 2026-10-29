use egui::{
    Align2, Color32, ColorImage, Id, Painter, Pos2, Rect, Stroke, StrokeKind, TextureFilter,
    TextureHandle, TextureOptions, pos2, vec2,
};
use mux_core::Provider;

use crate::icons;
use crate::theme::font;

// claude's signature orange, which its mark keeps even in a quiet row
pub const CLAUDE_ORANGE: Color32 = Color32::from_rgb(0xD9, 0x77, 0x57);

// draws an agent's logo: openai's from the icon font, claude's from its real mark, and opencode's painted by hand
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
            let texture = claude_mark(painter.ctx());
            painter.image(
                texture.id(),
                Rect::from_center_size(center, vec2(size, size)),
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                color,
            );
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

// claude's mark from simple icons, rendered white so it takes whatever colour it's drawn in, loaded once per context
fn claude_mark(ctx: &egui::Context) -> TextureHandle {
    let id = Id::unique("mux-claude-mark");
    if let Some(texture) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return texture;
    }
    let image = image::load_from_memory(include_bytes!("../../assets/marks/claude.png"))
        .map(|i| i.to_rgba8())
        .unwrap_or_default();
    let size = [image.width() as usize, image.height() as usize];
    let pixels = ColorImage::from_rgba_unmultiplied(size, image.as_raw());
    // drawn far smaller than it's stored, so mipmaps keep it smooth rather than jagged
    let options = TextureOptions::LINEAR.with_mipmap_mode(Some(TextureFilter::Linear));
    let texture = ctx.load_texture("claude-mark", pixels, options);
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    texture
}
