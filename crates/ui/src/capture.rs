use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;

use egui::{ColorImage, Context, Id, Vec2, ViewportCommand, vec2};

const SETTLE_SECONDS: f64 = 1.2;

// dev aid: with MUX_SCREENSHOT=path set, let animations settle, save the window as a png and quit
pub fn screenshot(ctx: &Context) {
    let Some(path) = std::env::var_os("MUX_SCREENSHOT") else {
        return;
    };
    let id = Id::unique("mux-capture");
    // MUX_SCREENSHOT_THEME picks light or dark, MUX_SCREENSHOT_ZOOM shrinks the ui to fit more in
    if ctx.cumulative_frame_nr() == 0 {
        match std::env::var("MUX_SCREENSHOT_THEME").as_deref() {
            Ok("light") => ctx.set_theme(egui::Theme::Light),
            Ok("dark") => ctx.set_theme(egui::Theme::Dark),
            _ => {}
        }
        if let Some(zoom) = std::env::var("MUX_SCREENSHOT_ZOOM")
            .ok()
            .and_then(|z| z.parse().ok())
        {
            ctx.set_zoom_factor(zoom);
        }
    }
    let requested = ctx.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    ctx.request_repaint();
    // MUX_SCREENSHOT_DELAY waits longer, for states that take a while to reach
    let settle = std::env::var("MUX_SCREENSHOT_DELAY")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(SETTLE_SECONDS);
    if requested || ctx.input(|i| i.time) < settle {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(id, true));
    let handle = ctx.clone();
    ctx.request_screenshot(move |image| {
        if let Err(err) = save(Path::new(&path), &image) {
            eprintln!("screenshot failed: {err}");
        }
        handle.send_viewport_cmd(ViewportCommand::Close);
        handle.request_repaint();
    });
}

// MUX_SCREENSHOT_SIZE=1200x1500 opens the window at that size, to fit more into one capture
pub fn size() -> Option<Vec2> {
    let value = std::env::var("MUX_SCREENSHOT_SIZE").ok()?;
    let (w, h) = value.split_once('x')?;
    Some(vec2(w.parse().ok()?, h.parse().ok()?))
}

pub(crate) fn save(path: &Path, image: &ColorImage) -> io::Result<()> {
    let [width, height] = image.size;
    let mut encoder = png::Encoder::new(
        BufWriter::new(File::create(path)?),
        width as u32,
        height as u32,
    );
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    let pixels: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|c| c.to_srgba_unmultiplied())
        .collect();
    writer.write_image_data(&pixels).map_err(io::Error::other)
}
