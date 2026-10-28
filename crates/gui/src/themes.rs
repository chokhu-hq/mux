use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Color32};
use mux_core::protocol;
use mux_ui::theme::{self, Palette};
use serde::{Deserialize, Serialize};

use crate::prefs::Prefs;

// a theme that can be picked, either one that ships with the app or one read from the themes folder
#[derive(Clone, Debug)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub dark: bool,
    pub custom: bool,
    pub palette: Palette,
}

// a theme file as people write it, where any colour left out comes from the theme it extends
#[derive(Serialize, Deserialize)]
struct File {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    appearance: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extends: Option<String>,
    #[serde(default)]
    colors: BTreeMap<String, String>,
    #[serde(default)]
    terminal: BTreeMap<String, String>,
}

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Dark,
    Light,
}

const TERMINAL: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "bright_black",
    "bright_red",
    "bright_green",
    "bright_yellow",
    "bright_blue",
    "bright_magenta",
    "bright_cyan",
    "bright_white",
];

fn colors(p: &mut Palette) -> [(&'static str, &mut Color32); 20] {
    [
        ("background", &mut p.bg),
        ("surface", &mut p.surface),
        ("sidebar", &mut p.sidebar),
        ("elevated", &mut p.elevated),
        ("hover", &mut p.hover),
        ("active", &mut p.active),
        ("border", &mut p.border),
        ("border_strong", &mut p.border_strong),
        ("text", &mut p.text),
        ("text_muted", &mut p.text_muted),
        ("text_faint", &mut p.text_faint),
        ("accent", &mut p.accent),
        ("accent_hover", &mut p.accent_hover),
        ("on_accent", &mut p.on_accent),
        ("success", &mut p.success),
        ("warning", &mut p.warning),
        ("danger", &mut p.danger),
        ("agent", &mut p.agent),
        ("shell", &mut p.shell),
        ("task", &mut p.task),
    ]
}

pub const MUX_DARK: &str = "mux-dark";
pub const MUX_LIGHT: &str = "mux-light";

// themes that ship with the app, written in the same format as anyone's own
const BUNDLED: [(&str, &str); 10] = [
    ("ayu-dark", include_str!("../themes/ayu-dark.json")),
    ("ayu-mirage", include_str!("../themes/ayu-mirage.json")),
    ("ayu-light", include_str!("../themes/ayu-light.json")),
    (
        "vscode-dark-2026",
        include_str!("../themes/vscode-dark-2026.json"),
    ),
    (
        "vscode-light-2026",
        include_str!("../themes/vscode-light-2026.json"),
    ),
    ("zedokai", include_str!("../themes/zedokai.json")),
    (
        "zedokai-darker",
        include_str!("../themes/zedokai-darker.json"),
    ),
    (
        "zedokai-light",
        include_str!("../themes/zedokai-light.json"),
    ),
    (
        "catppuccin-mocha",
        include_str!("../themes/catppuccin-mocha.json"),
    ),
    (
        "catppuccin-latte",
        include_str!("../themes/catppuccin-latte.json"),
    ),
];

// mux's own palettes stay in code because they're also what the app draws with before anything is read
fn bundled() -> Vec<Choice> {
    let mut list = vec![
        Choice {
            id: MUX_DARK.to_owned(),
            name: "Mux Dark".to_owned(),
            dark: true,
            custom: false,
            palette: theme::DARK,
        },
        Choice {
            id: MUX_LIGHT.to_owned(),
            name: "Mux Light".to_owned(),
            dark: false,
            custom: false,
            palette: theme::LIGHT,
        },
    ];
    for (id, json) in BUNDLED {
        match parse(id, json.as_bytes(), &list) {
            Ok(choice) => list.push(Choice {
                custom: false,
                ..choice
            }),
            Err(e) => log::error!("bundled theme {id}: {e}"),
        }
    }
    list
}

pub struct Themes {
    pub list: Vec<Choice>,
    bundled: Vec<Choice>,
    // theme files that couldn't be read, by file name, so whoever is writing one sees why
    pub errors: Vec<(String, String)>,
    stamps: Vec<(PathBuf, SystemTime)>,
    checked: Instant,
    // the theme under the pointer in a theme menu, shown in place of the picked one while it's there
    pub preview: Option<String>,
}

pub fn dir() -> PathBuf {
    protocol::data_dir().join("themes")
}

impl Themes {
    pub fn load() -> Self {
        let mut themes = Self {
            list: Vec::new(),
            bundled: bundled(),
            errors: Vec::new(),
            stamps: Vec::new(),
            checked: Instant::now(),
            preview: None,
        };
        themes.reload();
        themes
    }

    pub fn find(&self, id: &str) -> Option<&Choice> {
        self.list.iter().find(|c| c.id == id)
    }

    // custom themes come first so a file can replace a theme that ships with the app
    fn reload(&mut self) {
        let (custom, errors, stamps) = read_dir(&dir(), &self.bundled);
        self.list = custom;
        for choice in &self.bundled {
            if self.find(&choice.id).is_none() {
                self.list.push(choice.clone());
            }
        }
        self.list.sort_by_key(|c| c.custom);
        self.errors = errors;
        self.stamps = stamps;
    }

    // looks at the folder about once a second so a theme being written in an editor shows as it's saved
    pub fn refresh(&mut self) -> bool {
        if self.checked.elapsed() < Duration::from_secs(1) {
            return false;
        }
        self.checked = Instant::now();
        if stamps(&dir()) == self.stamps {
            return false;
        }
        self.reload();
        true
    }

    pub fn apply(&self, ctx: &egui::Context, prefs: &Prefs) {
        let pick = |id: Option<&str>, fallback: &str, dark: bool| {
            id.and_then(|id| self.find(id))
                .filter(|c| c.dark == dark)
                .or_else(|| self.find(fallback))
                .map_or(if dark { theme::DARK } else { theme::LIGHT }, |c| c.palette)
        };
        let mut dark = pick(prefs.dark_theme.as_deref(), MUX_DARK, true);
        let mut light = pick(prefs.light_theme.as_deref(), MUX_LIGHT, false);
        if let Some(c) = self.preview.as_deref().and_then(|id| self.find(id)) {
            if c.dark {
                dark = c.palette;
            } else {
                light = c.palette;
            }
        }
        // dev aid: MUX_SCREENSHOT_THEME can name any theme for a capture
        if let Some(c) = std::env::var("MUX_SCREENSHOT_THEME")
            .ok()
            .and_then(|id| self.find(&id))
        {
            if c.dark {
                dark = c.palette;
            } else {
                light = c.palette;
            }
            ctx.set_theme(if c.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
        }
        theme::set_palettes(ctx, dark, light);
    }

    // writes a copy of a theme into the folder as a starting point and returns its id
    pub fn duplicate(&mut self, id: &str) -> std::io::Result<String> {
        let source = self
            .find(id)
            .cloned()
            .ok_or_else(|| std::io::Error::other("no such theme"))?;
        let dir = dir();
        std::fs::create_dir_all(&dir)?;
        let base = format!("{}-custom", slug(&source.name));
        let mut stem = base.clone();
        let mut n = 2;
        while dir.join(format!("{stem}.json")).exists() || self.find(&stem).is_some() {
            stem = format!("{base}-{n}");
            n += 1;
        }
        let mut palette = source.palette;
        let file = File {
            name: format!("{} Custom", source.name),
            appearance: Some(if source.dark { Mode::Dark } else { Mode::Light }),
            extends: None,
            colors: colors(&mut palette)
                .into_iter()
                .map(|(name, c)| (name.to_owned(), to_hex(*c)))
                .collect(),
            terminal: TERMINAL
                .iter()
                .zip(source.palette.ansi)
                .map(|(name, c)| ((*name).to_owned(), to_hex(c)))
                .collect(),
        };
        let json = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        std::fs::write(dir.join(format!("{stem}.json")), json)?;
        self.reload();
        Ok(stem)
    }
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
}

fn stamps(dir: &Path) -> Vec<(PathBuf, SystemTime)> {
    json_files(dir)
        .into_iter()
        .map(|p| {
            let modified = std::fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (p, modified)
        })
        .collect()
}

type Read = (
    Vec<Choice>,
    Vec<(String, String)>,
    Vec<(PathBuf, SystemTime)>,
);

fn read_dir(dir: &Path, bases: &[Choice]) -> Read {
    let stamps = stamps(dir);
    let mut list = Vec::new();
    let mut errors = Vec::new();
    for (path, _) in &stamps {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let id = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parsed = std::fs::read(path)
            .map_err(|e| e.to_string())
            .and_then(|bytes| parse(&id, &bytes, bases));
        match parsed {
            Ok(choice) => list.push(choice),
            Err(e) => {
                log::warn!("theme {file_name}: {e}");
                errors.push((file_name, e));
            }
        }
    }
    (list, errors, stamps)
}

// a theme can extend any that ships with the app
fn parse(id: &str, bytes: &[u8], bases: &[Choice]) -> Result<Choice, String> {
    let file: File = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    let base = match &file.extends {
        Some(name) => Some(
            bases
                .iter()
                .find(|b| &b.id == name)
                .ok_or_else(|| format!("no theme called {name} to extend"))?,
        ),
        None => None,
    };
    let dark = match (file.appearance, base) {
        (Some(mode), _) => mode == Mode::Dark,
        (None, Some(base)) => base.dark,
        (None, None) => return Err("appearance must be \"dark\" or \"light\"".to_owned()),
    };
    let mut palette = base.map_or(if dark { theme::DARK } else { theme::LIGHT }, |b| b.palette);
    for (key, value) in &file.colors {
        let mut fields = colors(&mut palette);
        let slot = fields
            .iter_mut()
            .find(|(name, _)| name == key)
            .ok_or_else(|| format!("unknown color {key}"))?;
        *slot.1 = from_hex(value).ok_or_else(|| format!("{key} isn't a hex color: {value}"))?;
    }
    for (key, value) in &file.terminal {
        let index = TERMINAL
            .iter()
            .position(|name| name == key)
            .ok_or_else(|| format!("unknown terminal color {key}"))?;
        palette.ansi[index] =
            from_hex(value).ok_or_else(|| format!("{key} isn't a hex color: {value}"))?;
    }
    Ok(Choice {
        id: id.to_owned(),
        name: file.name,
        dark,
        custom: true,
        palette,
    })
}

fn from_hex(value: &str) -> Option<Color32> {
    let hex = value.trim().trim_start_matches('#');
    let hex: String = match hex.len() {
        3 | 4 => hex.chars().flat_map(|c| [c, c]).collect(),
        6 | 8 => hex.to_owned(),
        _ => return None,
    };
    let n = u32::from_str_radix(&hex, 16).ok()?;
    Some(if hex.len() == 8 {
        Color32::from_rgba_unmultiplied((n >> 24) as u8, (n >> 16) as u8, (n >> 8) as u8, n as u8)
    } else {
        Color32::from_rgb((n >> 16) as u8, (n >> 8) as u8, n as u8)
    })
}

fn to_hex(c: Color32) -> String {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    if a == u8::MAX {
        format!("#{r:02X}{g:02X}{b:02X}")
    } else {
        format!("#{r:02X}{g:02X}{b:02X}{a:02X}")
    }
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_file_fills_gaps_from_what_it_extends() {
        let json = br##"{
            "name": "Mirage Red",
            "extends": "ayu-mirage",
            "colors": { "accent": "#f00" },
            "terminal": { "bright_white": "#ffffff80" }
        }"##;
        let bundled = bundled();
        let choice = parse("mirage-red", json, &bundled).unwrap();
        let mirage = bundled.iter().find(|c| c.id == "ayu-mirage").unwrap();
        assert!(choice.dark);
        assert_eq!(choice.palette.accent, Color32::from_rgb(255, 0, 0));
        assert_eq!(choice.palette.bg, mirage.palette.bg);
        assert_eq!(choice.palette.ansi[15].a(), 128);
    }

    #[test]
    fn every_bundled_theme_reads() {
        assert_eq!(bundled().len(), BUNDLED.len() + 2);
    }

    #[test]
    fn mistakes_in_a_theme_file_are_named() {
        let unknown = br##"{"name": "x", "appearance": "dark", "colors": {"accnt": "#fff"}}"##;
        assert_eq!(parse("x", unknown, &[]).unwrap_err(), "unknown color accnt");
        let mode = br#"{"name": "x"}"#;
        assert!(parse("x", mode, &[]).unwrap_err().contains("appearance"));
        let bad = br#"{"name": "x", "appearance": "light", "colors": {"text": "blue"}}"#;
        assert!(parse("x", bad, &[]).unwrap_err().contains("hex"));
    }

    #[test]
    fn a_written_theme_reads_back_the_same() {
        let mut palette = bundled()
            .into_iter()
            .find(|c| c.id == "zedokai")
            .unwrap()
            .palette;
        let file = File {
            name: "Copy".to_owned(),
            appearance: Some(Mode::Dark),
            extends: None,
            colors: colors(&mut palette)
                .into_iter()
                .map(|(n, c)| (n.to_owned(), to_hex(*c)))
                .collect(),
            terminal: TERMINAL
                .iter()
                .zip(palette.ansi)
                .map(|(n, c)| ((*n).to_owned(), to_hex(c)))
                .collect(),
        };
        let json = serde_json::to_vec(&file).unwrap();
        assert_eq!(parse("copy", &json, &[]).unwrap().palette, palette);
        assert_eq!(slug("VS Code Dark 2026"), "vs-code-dark-2026");
    }
}
