#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use eframe::egui;

// a desktop notification for an agent, and the session to open when it's clicked
#[cfg(target_os = "macos")]
pub fn send(title: &str, body: &str, id: u64, clicked: Sender<u64>, ctx: egui::Context) {
    use mac_notification_sys::{Notification, NotificationResponse};

    static CREDITED: std::sync::Once = std::sync::Once::new();
    let (title, body) = (title.to_owned(), body.to_owned());
    // waiting for the click blocks, so each notification gets its own thread
    std::thread::spawn(move || {
        // macos shows a notification under an installed app, so a build run outside Mux.app borrows terminal's
        CREDITED.call_once(|| {
            let app = mac_notification_sys::get_bundle_identifier("Mux")
                .unwrap_or_else(|| "com.apple.Terminal".to_owned());
            let _ = mac_notification_sys::set_application(&app);
        });
        let response = Notification::new()
            .title(&title)
            .message(&body)
            .wait_for_click(true)
            .send();
        if let Ok(NotificationResponse::Click) = response {
            let _ = clicked.send(id);
            ctx.request_repaint();
        }
    });
}

// a desktop notification for an agent, and the session to open when it's clicked
#[cfg(target_os = "linux")]
pub fn send(title: &str, body: &str, id: u64, clicked: Sender<u64>, ctx: egui::Context) {
    let (title, body) = (title.to_owned(), body.to_owned());
    // waiting for the click blocks, so each notification gets its own thread
    std::thread::spawn(move || {
        let mut command = Command::new("notify-send");
        command
            .args(["--app-name=Mux", "--hint=string:desktop-entry:mux"])
            .arg("--action=default=Open")
            .arg("--wait");
        if let Some(icon) = icon() {
            command.arg(format!("--icon={icon}"));
        }
        let Ok(output) = command
            .arg(&title)
            .arg(&body)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            return;
        };
        if String::from_utf8_lossy(&output.stdout).trim() == "default" {
            let _ = clicked.send(id);
            ctx.request_repaint();
        }
    });
}

// the app icon written next to mux's data once, since a notification needs a file to show
#[cfg(target_os = "linux")]
fn icon() -> Option<String> {
    let path = mux_core::protocol::data_dir().join("icon.png");
    if !path.exists() {
        std::fs::create_dir_all(path.parent()?).ok()?;
        std::fs::write(&path, include_bytes!("../../../assets/icons/mux-128.png")).ok()?;
    }
    Some(path.to_string_lossy().into_owned())
}
