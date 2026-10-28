use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};

use eframe::egui;
use serde_json::Value;

const REPO: &str = "https://api.github.com/repos/chokhu-hq/mux";
const RELEASES: &str = "https://api.github.com/repos/chokhu-hq/mux/releases";

// release builds learn their channel and commit from ci, and a build from source never checks
const CHANNEL: Option<&str> = option_env!("MUX_CHANNEL");
const COMMIT: Option<&str> = option_env!("MUX_COMMIT");

pub struct Update {
    pub version: String,
    pub url: String,
}

// asks github once in the background whether a newer build of this channel is out
pub fn check(ctx: &egui::Context) -> Receiver<Update> {
    let (tx, rx) = mpsc::channel();
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        if CHANNEL.is_none() {
            return;
        }
        if let Some(update) = newer() {
            log::info!("{} is available", update.version);
            let _ = tx.send(update);
            ctx.request_repaint();
        }
    });
    rx
}

// the release this build came from, as a name for its server in wsl and the place to download it
#[cfg(windows)]
pub fn server_release() -> Option<(String, String)> {
    let base = "https://github.com/chokhu-hq/mux/releases/download";
    match (CHANNEL?, COMMIT) {
        ("experimental", Some(commit)) => Some((
            format!("experimental-{}", &commit[..commit.len().min(7)]),
            format!("{base}/experimental"),
        )),
        ("experimental", None) => None,
        _ => {
            let version = env!("CARGO_PKG_VERSION");
            Some((format!("v{version}"), format!("{base}/v{version}")))
        }
    }
}

// how this build was made, for the log and for anyone reading it
pub fn build() -> String {
    match (CHANNEL, COMMIT) {
        (Some(channel), Some(commit)) => format!("{channel} {}", &commit[..commit.len().min(7)]),
        (Some(channel), None) => channel.to_owned(),
        _ => "built from source".to_owned(),
    }
}

fn newer() -> Option<Update> {
    match CHANNEL? {
        // experimental is rebuilt in place, so a different commit behind it is the update
        "experimental" => {
            let release = fetch(&format!("{RELEASES}/tags/experimental"))?;
            let commit = release["target_commitish"].as_str()?;
            let url = release["html_url"].as_str()?.to_owned();
            // the release can lag behind this build while a newer one is still being made, so only a later commit counts
            let ours = COMMIT?;
            if commit == ours {
                return None;
            }
            let compared = fetch(&format!("{REPO}/compare/{ours}...{commit}"))?;
            (compared["status"] == "ahead").then(|| Update {
                version: format!("Experimental {}", &commit[..commit.len().min(7)]),
                url,
            })
        }
        _ => {
            let release = fetch(&format!("{RELEASES}/latest"))?;
            let tag = release["tag_name"].as_str()?;
            let url = release["html_url"].as_str()?.to_owned();
            is_newer(tag, env!("CARGO_PKG_VERSION")).then(|| Update {
                version: tag.to_owned(),
                url,
            })
        }
    }
}

// curl ships with macos, windows and nearly every linux, which saves bundling an http client
fn fetch(url: &str) -> Option<Value> {
    let output = mux_core::command("curl")
        .args(["-fsSL", "--max-time", "15"])
        .args(["-H", "Accept: application/vnd.github+json"])
        .arg(url)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        log::warn!("update check couldn't reach {url}");
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

// compares dotted versions number by number, ignoring a leading v
fn is_newer(tag: &str, current: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.trim_start_matches('v')
            .split(['.', '-'])
            .map_while(|p| p.parse().ok())
            .collect()
    };
    parts(tag) > parts(current)
}

#[cfg(test)]
mod tests {
    use super::is_newer;

    #[test]
    fn compares_versions_by_number() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.3"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
    }
}
