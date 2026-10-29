use std::process::Stdio;
use std::sync::mpsc::{self, Receiver};

use eframe::egui;
use serde_json::Value;

const REPO: &str = "https://api.github.com/repos/chokhu-hq/mux";
const RELEASES: &str = "https://api.github.com/repos/chokhu-hq/mux/releases";

// release builds learn their channel and commit from ci, and a build from source never checks
const CHANNEL: Option<&str> = option_env!("MUX_CHANNEL");
const COMMIT: Option<&str> = option_env!("MUX_COMMIT");

#[derive(Clone)]
pub struct Update {
    pub version: String,
    pub url: String,
    // this platform's build in the release, when it's there and signed
    pub download: Option<Download>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub name: String,
    pub url: String,
    pub signature: String,
    pub size: u64,
}

// the release file holding this platform's build
fn asset_name() -> String {
    if cfg!(target_os = "macos") {
        "mux-macos-universal.zip".to_owned()
    } else if cfg!(windows) {
        "mux-windows-x86_64.zip".to_owned()
    } else {
        format!("mux-linux-{}.tar.gz", std::env::consts::ARCH)
    }
}

// a build without a signature beside it is left for downloading by hand
fn download(release: &Value, name: &str) -> Option<Download> {
    let assets = release["assets"].as_array()?;
    let find = |wanted: &str| assets.iter().find(|a| a["name"] == wanted);
    let build = find(name)?;
    let signature = find(&format!("{name}.sig"))?;
    Some(Download {
        name: name.to_owned(),
        url: build["browser_download_url"].as_str()?.to_owned(),
        signature: signature["browser_download_url"].as_str()?.to_owned(),
        size: build["size"].as_u64().unwrap_or(0),
    })
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

// only builds made in ci check for and install updates, never one built from source
pub fn is_release() -> bool {
    CHANNEL.is_some()
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
                download: download(&release, &asset_name()),
            })
        }
        _ => {
            let release = fetch(&format!("{RELEASES}/latest"))?;
            let tag = release["tag_name"].as_str()?;
            let url = release["html_url"].as_str()?.to_owned();
            is_newer(tag, env!("CARGO_PKG_VERSION")).then(|| Update {
                version: tag.to_owned(),
                url,
                download: download(&release, &asset_name()),
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
    use super::{Download, download, is_newer};
    use serde_json::json;

    #[test]
    fn picks_this_platforms_signed_build() {
        let asset = |name: &str| json!({ "name": name, "browser_download_url": format!("https://x/{name}"), "size": 42 });
        let release = json!({ "assets": [
            asset("mux-linux-x86_64.tar.gz"), asset("mux-linux-x86_64.tar.gz.sig"),
            asset("mux-windows-x86_64.zip"),
        ] });
        assert_eq!(
            download(&release, "mux-linux-x86_64.tar.gz"),
            Some(Download {
                name: "mux-linux-x86_64.tar.gz".into(),
                url: "https://x/mux-linux-x86_64.tar.gz".into(),
                signature: "https://x/mux-linux-x86_64.tar.gz.sig".into(),
                size: 42,
            })
        );
        assert_eq!(
            download(&release, "mux-windows-x86_64.zip"),
            None,
            "unsigned"
        );
        assert_eq!(
            download(&release, "mux-macos-universal.zip"),
            None,
            "missing"
        );
    }

    #[test]
    fn compares_versions_by_number() {
        assert!(is_newer("v0.2.0", "0.1.0"));
        assert!(is_newer("v0.10.0", "0.9.3"));
        assert!(!is_newer("v0.1.0", "0.1.0"));
        assert!(!is_newer("v0.1.0", "0.2.0"));
    }
}
