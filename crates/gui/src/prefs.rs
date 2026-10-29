use std::path::PathBuf;

use eframe::egui;
use mux_core::protocol::{self, AgentSettings};
use mux_core::{Access, Provider};
use mux_ui::motion;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

// how much of the blurred desktop shows through, where off keeps every surface solid
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transparency {
    Off,
    Subtle,
    Full,
}

// on linux the compositor's blur is well tried, while windows and macos start solid until it's turned on
impl Default for Transparency {
    fn default() -> Self {
        if cfg!(target_os = "linux") {
            Self::Subtle
        } else {
            Self::Off
        }
    }
}

impl Transparency {
    // how solid the sidebar and the content area are painted
    pub fn opacity(self) -> (f32, f32) {
        match self {
            Self::Off => (1.0, 1.0),
            Self::Subtle => (0.82, 0.94),
            Self::Full => (0.7, 0.84),
        }
    }
}

// choices that belong to this window rather than the server, saved next to the server's data
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Prefs {
    pub appearance: Appearance,
    pub reduced_motion: bool,
    // saved under a new name, since every older file says subtle, which windows and macos shouldn't take up
    #[serde(default, rename = "see_through")]
    #[cfg_attr(target_os = "linux", serde(alias = "transparency"))]
    pub transparency: Transparency,
    // on windows, the wsl distribution the server runs in, or none to run it on windows itself
    #[serde(default)]
    pub wsl: Option<String>,
    #[serde(default)]
    pub new_agent: NewAgent,
    // terminal text size, or none for the default
    #[serde(default)]
    pub terminal_text: Option<f32>,
    // the themes used in dark and light mode, or none for mux's own
    #[serde(default)]
    pub dark_theme: Option<String>,
    #[serde(default)]
    pub light_theme: Option<String>,
    // new builds download and go in place by themselves unless this is set, leaving only a restart
    #[serde(default)]
    pub manual_updates: bool,
}

// what a new agent starts with, before anything is picked in its composer
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct NewAgent {
    pub provider: Provider,
    // a model of that agent, or none for the one its cli starts with
    pub model: Option<String>,
    pub access: Access,
}

impl NewAgent {
    pub fn settings(&self) -> AgentSettings {
        AgentSettings {
            provider: self.provider,
            model: self.model.clone(),
            access: self.access,
            ..AgentSettings::default()
        }
    }
}

impl Prefs {
    pub fn terminal_size(&self) -> f32 {
        self.terminal_text.unwrap_or(mux_ui::theme::text::MONO)
    }

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

#[cfg(test)]
mod tests {
    use super::{Prefs, Transparency};

    #[test]
    fn transparency_reads_back_under_its_new_name() {
        let saved = serde_json::to_string(&Prefs {
            transparency: Transparency::Full,
            ..Prefs::default()
        })
        .unwrap();
        assert!(saved.contains("\"see_through\":\"Full\""));
        let read: Prefs = serde_json::from_str(&saved).unwrap();
        assert_eq!(read.transparency, Transparency::Full);
        // an older file's choice still counts on linux, where it was offered
        let old: Prefs = serde_json::from_str(
            r#"{"appearance":"System","reduced_motion":false,"transparency":"Off"}"#,
        )
        .unwrap();
        let expected = if cfg!(target_os = "linux") {
            Transparency::Off
        } else {
            Transparency::default()
        };
        assert_eq!(old.transparency, expected);
    }
}
