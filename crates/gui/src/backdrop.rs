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

// windows 11 draws its acrylic light or dark by the window's own dark mode, which starts out light whatever
// the app looks like, so a dark mux would otherwise sit on a pale frost that greys everything out
#[cfg(windows)]
pub fn set_dark(window: &impl HasWindowHandle, dark: bool) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let value: i32 = dark.into();
    // the handle is the live window winit owns, and the value is read during the call
    unsafe {
        DwmSetWindowAttribute(
            handle.hwnd.get() as _,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&value as *const i32).cast(),
            std::mem::size_of::<i32>() as u32,
        );
    }
}
