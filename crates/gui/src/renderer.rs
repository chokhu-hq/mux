#[cfg(windows)]
use eframe::egui;

// a start with the see-through renderer is noted down first and the note is cleared once frames are drawing,
// so a gpu that crashes or hangs with it gets the standard renderer next time, until mux is updated and it's tried again,
// and MUX_RENDERER=standard skips it for a gpu that draws a blank window, which the app itself can't notice
#[cfg(windows)]
pub(crate) fn try_see_through() -> bool {
    if std::env::var("MUX_RENDERER").is_ok_and(|v| v == "standard") {
        log::info!("using the standard renderer, as MUX_RENDERER asks");
        return false;
    }
    let marker = renderer_marker();
    let build = mux_core::build_id();
    if std::fs::read_to_string(&marker).is_ok_and(|last| last == build) {
        log::warn!(
            "the see-through renderer didn't get going last time, so this start uses the standard one"
        );
        return false;
    }
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&marker, build);
    true
}

#[cfg(windows)]
pub(crate) fn renderer_marker() -> std::path::PathBuf {
    mux_core::protocol::data_dir().join("renderer-check")
}

// the see-through renderer has drawn enough frames to trust, so the next start uses it again
#[cfg(windows)]
pub(crate) fn renderer_works(ctx: &egui::Context) {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if see_through_renderer()
        && ctx.cumulative_frame_nr() > 30
        && !DONE.swap(true, std::sync::atomic::Ordering::Relaxed)
    {
        let _ = std::fs::remove_file(renderer_marker());
    }
}

// whether the window can be drawn see-through, which on windows takes a renderer set up for it at start
pub(crate) static SEE_THROUGH: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

pub(crate) fn see_through_renderer() -> bool {
    SEE_THROUGH.load(std::sync::atomic::Ordering::Relaxed)
}

// windows only composites a see-through window when direct3d 12 draws into a directcomposition visual,
// and vulkan, which wgpu would otherwise pick first, can't, so windows asks for exactly that
pub(crate) fn renderer() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut config = eframe::egui_wgpu::WgpuConfiguration::default();
    if cfg!(windows)
        && see_through_renderer()
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup
    {
        setup.instance_descriptor.backends = eframe::wgpu::Backends::DX12;
        setup
            .instance_descriptor
            .backend_options
            .dx12
            .presentation_system = eframe::wgpu::Dx12SwapchainKind::DxgiFromVisual;
    }
    config
}
