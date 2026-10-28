use std::path::PathBuf;

use eframe::egui;
use mux_core::protocol;
use mux_ui::motion;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

// choices that belong to this window rather than the server, saved next to the server's data
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    pub appearance: Appearance,
    pub reduced_motion: bool,
}

impl Prefs {
    pub fn load() -> Self {
        std::fs::read(path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let path = path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, bytes);
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.appearance {
            Appearance::System => egui::ThemePreference::System,
            Appearance::Light => egui::ThemePreference::Light,
            Appearance::Dark => egui::ThemePreference::Dark,
        });
        motion::set_reduced(ctx, self.reduced_motion);
    }
}

fn path() -> PathBuf {
    protocol::data_dir().join("ui.json")
}
