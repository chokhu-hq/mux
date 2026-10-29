use raw_window_handle::HasWindowHandle;

// the frosted background the system draws behind the window, which shows through wherever mux paints see-through
pub fn apply(window: &impl HasWindowHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        // older windows only has an acrylic that lags while the window is dragged, so it's left solid there
        if mux_core::windows_build().is_none_or(|build| build < 22621) {
            return Err("frosted backgrounds need Windows 11 22H2 or newer".to_owned());
        }
        window_vibrancy::apply_acrylic(window, None).map_err(|e| e.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        use window_vibrancy::{NSVisualEffectMaterial, NSVisualEffectState};
        // active keeps it frosted when the window isn't focused, the way a sidebar stays
        window_vibrancy::apply_vibrancy(
            window,
            NSVisualEffectMaterial::UnderWindowBackground,
            Some(NSVisualEffectState::Active),
            None,
        )
        .map_err(|e| e.to_string())
    }
}

pub fn clear(window: &impl HasWindowHandle) {
    #[cfg(windows)]
    let cleared = window_vibrancy::clear_acrylic(window).map(|_| ());
    #[cfg(target_os = "macos")]
    let cleared = window_vibrancy::clear_vibrancy(window).map(|_| ());
    if let Err(e) = cleared {
        log::warn!("couldn't take the frosted background away: {e}");
    }
}
