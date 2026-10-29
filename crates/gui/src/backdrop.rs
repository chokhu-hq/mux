use raw_window_handle::HasWindowHandle;

// the frosted background the system draws behind the window, which shows through wherever mux paints see-through
pub fn apply(window: &impl HasWindowHandle) -> Result<(), String> {
    #[cfg(windows)]
    {
        // older windows only has an acrylic that lags while the window is dragged, so it's left solid there
        if mux_core::windows_build().is_none_or(|build| build < 22621) {
            return Err("frosted backgrounds need Windows 11 22H2 or newer".to_owned());
        }
        // which way to ask windows for it, so a machine where one shows nothing can try another without a new build
        let way = std::env::var("MUX_BACKDROP").unwrap_or_default();
        log::info!(
            "frosted background on windows build {:?}, asked for as {:?}",
            mux_core::windows_build(),
            if way.is_empty() { "acrylic" } else { &way }
        );
        match way.as_str() {
            "blur" => window_vibrancy::apply_blur(window, None).map_err(|e| e.to_string()),
            "plain" => window_vibrancy::apply_acrylic(window, None).map_err(|e| e.to_string()),
            _ => {
                // windows only draws a system backdrop where its frame is, so the frame is stretched over the whole window
                extend_frame(window, -1);
                window_vibrancy::apply_acrylic(window, None).map_err(|e| e.to_string())
            }
        }
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
    let cleared = if std::env::var("MUX_BACKDROP").is_ok_and(|way| way == "blur") {
        window_vibrancy::clear_blur(window)
    } else {
        extend_frame(window, 0);
        window_vibrancy::clear_acrylic(window)
    };
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

#[cfg(windows)]
fn extend_frame(window: &impl HasWindowHandle, margin: i32) {
    use raw_window_handle::RawWindowHandle;
    use windows_sys::Win32::Graphics::Dwm::DwmExtendFrameIntoClientArea;
    use windows_sys::Win32::UI::Controls::MARGINS;
    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return;
    };
    let margins = MARGINS {
        cxLeftWidth: margin,
        cxRightWidth: margin,
        cyTopHeight: margin,
        cyBottomHeight: margin,
    };
    // the handle is the live window winit owns, and the margins are read during the call
    let result = unsafe { DwmExtendFrameIntoClientArea(handle.hwnd.get() as _, &margins) };
    log::info!("stretched the window frame to {margin}, result {result:#x}");
}
