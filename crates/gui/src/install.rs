use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use eframe::egui;

use crate::update::Download;

// the release key's public half, so only builds signed in ci are ever installed
const PUBLIC_KEY: [u8; 32] = [
    71, 195, 190, 225, 69, 70, 240, 35, 174, 183, 37, 244, 236, 68, 147, 38, 203, 178, 121, 61, 69,
    187, 145, 46, 148, 1, 232, 4, 71, 0, 140, 64,
];

#[derive(Clone, Debug, PartialEq)]
pub enum Stage {
    Downloading { done: u64, total: u64 },
    // the new build is in place and runs on the next start
    Ready,
    Failed(String),
}

// an update being downloaded and put in place on a thread of its own
pub struct Installer {
    stage: Arc<Mutex<Stage>>,
}

impl Installer {
    pub fn start(ctx: &egui::Context, download: Download) -> Self {
        let stage = Arc::new(Mutex::new(Stage::Downloading {
            done: 0,
            total: download.size,
        }));
        let (shared, ctx) = (stage.clone(), ctx.clone());
        std::thread::spawn(move || {
            let set = |next: Stage| {
                if let Ok(mut stage) = shared.lock() {
                    *stage = next;
                }
                ctx.request_repaint();
            };
            match install(&download, &set) {
                Ok(()) => {
                    log::info!("installed the update from {}", download.url);
                    set(Stage::Ready);
                }
                Err(e) => {
                    log::warn!("couldn't install the update: {e}");
                    set(Stage::Failed(e.to_string()));
                }
            }
        });
        Self { stage }
    }

    pub fn stage(&self) -> Stage {
        self.stage
            .lock()
            .map(|s| s.clone())
            .unwrap_or(Stage::Failed("The updater stopped".to_owned()))
    }
}

// what gets replaced: the binary on linux and windows, the whole app bundle on macos,
// read once at start since a linux binary renamed over reads back as "(deleted)"
pub fn target() -> Result<&'static Path, &'static str> {
    static TARGET: OnceLock<Result<PathBuf, &'static str>> = OnceLock::new();
    TARGET
        .get_or_init(|| {
            let exe = std::env::current_exe()
                .and_then(std::fs::canonicalize)
                .map_err(|_| "Mux couldn't tell where it's installed")?;
            let target = if cfg!(target_os = "macos") {
                // a download opened in place runs from a hidden read-only copy that can't be updated
                if exe.to_string_lossy().contains("/AppTranslocation/") {
                    return Err("Move Mux to Applications to update it");
                }
                exe.ancestors()
                    .nth(3)
                    .filter(|b| b.extension().is_some_and(|e| e == "app"))
                    .ok_or("Mux isn't running from an app bundle")?
                    .to_path_buf()
            } else {
                exe
            };
            let dir = target
                .parent()
                .ok_or("Mux couldn't tell where it's installed")?;
            if !writable(dir) {
                return Err("Mux can't write to the folder it's installed in");
            }
            Ok(target)
        })
        .as_deref()
        .map_err(|e| *e)
}

fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".mux-write-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn work_dir() -> PathBuf {
    mux_core::protocol::data_dir().join("update")
}

// leftovers from the last update, which couldn't go while the old build was still running
pub fn clean_up() {
    let _ = std::fs::remove_dir_all(work_dir());
    if let Ok(target) = target() {
        let _ = std::fs::remove_file(old(target));
        let _ = std::fs::remove_dir_all(old(target));
    }
}

fn old(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target.with_file_name(format!(".{name}.old"))
}

fn install(download: &Download, set: &dyn Fn(Stage)) -> io::Result<()> {
    let target = target().map_err(io::Error::other)?;
    install_to(download, target, &work_dir(), &PUBLIC_KEY, set)
}

fn install_to(
    download: &Download,
    target: &Path,
    dir: &Path,
    key: &[u8; 32],
    set: &dyn Fn(Stage),
) -> io::Result<()> {
    let _ = std::fs::remove_dir_all(dir);
    std::fs::create_dir_all(dir)?;
    let archive = dir.join(&download.name);
    fetch(&download.url, &archive, download.size, set)?;
    let signature = dir.join(format!("{}.sig", download.name));
    fetch(&download.signature, &signature, 0, &|_| {})?;
    verify(&std::fs::read(&archive)?, &std::fs::read(&signature)?, key)?;
    let unpacked = dir.join("unpacked");
    std::fs::create_dir_all(&unpacked)?;
    // tar comes with linux, macos and windows 10 on, and reads zip files as well as tarballs
    run(mux_core::command("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&unpacked))?;
    swap(target, &unpacked)
}

// downloads with curl, reporting how much has arrived while it runs
fn fetch(url: &str, to: &Path, total: u64, set: &dyn Fn(Stage)) -> io::Result<()> {
    let mut child = mux_core::command("curl")
        .args(["-fsSL", "--max-time", "900", "-o"])
        .arg(to)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            return Err(io::Error::other("The download didn't finish"));
        }
        if total > 0 {
            let done = std::fs::metadata(to).map_or(0, |m| m.len());
            set(Stage::Downloading { done, total });
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn verify(bytes: &[u8], signature: &[u8], key: &[u8; 32]) -> io::Result<()> {
    let bad =
        || io::Error::other("The download didn't match its signature, so it wasn't installed");
    let key = ed25519_compact::PublicKey::from_slice(key).map_err(|_| bad())?;
    let signature = ed25519_compact::Signature::from_slice(signature).map_err(|_| bad())?;
    key.verify(bytes, &signature).map_err(|_| bad())
}

fn run(command: &mut std::process::Command) -> io::Result<()> {
    let output = command.stdin(Stdio::null()).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "Unpacking the update failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

// the new build goes in beside the old and is renamed over it, so a failure part way leaves the old one working
fn swap(target: &Path, unpacked: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let bundle = unpacked.join("Mux.app");
        let staged = target.with_file_name(".Mux-update.app");
        let _ = std::fs::remove_dir_all(&staged);
        // ditto copies across volumes and keeps the bundle's signature and permissions
        run(mux_core::command("ditto").arg(&bundle).arg(&staged))?;
        let previous = old(target);
        let _ = std::fs::remove_dir_all(&previous);
        std::fs::rename(target, &previous)?;
        if let Err(e) = std::fs::rename(&staged, target) {
            let _ = std::fs::rename(&previous, target);
            return Err(e);
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let new = unpacked.join("mux-windows-x86_64").join("Mux.exe");
        // a running exe can't be overwritten but can be moved aside, and it's deleted on the next start
        let previous = old(target);
        let _ = std::fs::remove_file(&previous);
        std::fs::rename(target, &previous)?;
        if let Err(e) = std::fs::copy(&new, target) {
            let _ = std::fs::rename(&previous, target);
            return Err(e);
        }
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        use std::os::unix::fs::PermissionsExt;
        let new = unpacked
            .join(format!("mux-linux-{}", std::env::consts::ARCH))
            .join("mux-gui");
        let part = target.with_file_name(".mux.part");
        std::fs::copy(&new, &part)?;
        std::fs::set_permissions(&part, std::fs::Permissions::from_mode(0o755))?;
        // the running app keeps its old file open, so renaming over it is safe
        std::fs::rename(&part, target)
    }
}

// opens the new build and closes this one
pub fn restart(ctx: &egui::Context) {
    let Ok(target) = target() else {
        return;
    };
    let launched = if cfg!(target_os = "macos") {
        mux_core::command("open").arg("-n").arg(target).spawn()
    } else {
        std::process::Command::new(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    };
    match launched {
        Ok(_) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        Err(e) => log::warn!("couldn't start the updated build: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_signed_download_passes() {
        let pair = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([7; 32]));
        let key: [u8; 32] = pair.pk.as_ref().try_into().unwrap();
        let bytes = b"a build";
        let signature = pair.sk.sign(bytes, None);
        assert!(verify(bytes, signature.as_ref(), &key).is_ok());
        assert!(verify(b"a tampered build", signature.as_ref(), &key).is_err());
        assert!(verify(bytes, &[0; 64], &key).is_err());
        assert!(verify(bytes, b"short", &key).is_err());
    }

    // the whole path a linux update takes, from a signed tarball at a url to the binary replaced in place
    #[cfg(target_os = "linux")]
    #[test]
    fn a_signed_build_replaces_the_installed_one() {
        let root = std::env::temp_dir().join(format!("mux-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let release = root.join("release");
        let build = release.join(format!("mux-linux-{}", std::env::consts::ARCH));
        std::fs::create_dir_all(&build).unwrap();
        std::fs::write(build.join("mux-gui"), b"new build").unwrap();
        let name = format!("mux-linux-{}.tar.gz", std::env::consts::ARCH);
        let archive = release.join(&name);
        let made = std::process::Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&release)
            .arg(build.file_name().unwrap())
            .status()
            .unwrap();
        assert!(made.success());
        let pair = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([9; 32]));
        let key: [u8; 32] = pair.pk.as_ref().try_into().unwrap();
        let signature = pair.sk.sign(std::fs::read(&archive).unwrap(), None);
        std::fs::write(release.join(format!("{name}.sig")), signature.as_ref()).unwrap();

        let installed = root.join("bin").join("mux");
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"old build").unwrap();
        let download = Download {
            name: name.clone(),
            url: format!("file://{}", archive.display()),
            signature: format!("file://{}.sig", archive.display()),
            size: std::fs::metadata(&archive).unwrap().len(),
        };
        install_to(&download, &installed, &root.join("work"), &key, &|_| {}).unwrap();
        assert_eq!(std::fs::read(&installed).unwrap(), b"new build");

        // a signature from any other key leaves the installed build alone
        std::fs::write(&installed, b"old build").unwrap();
        let other = ed25519_compact::KeyPair::from_seed(ed25519_compact::Seed::new([1; 32]));
        let other: [u8; 32] = other.pk.as_ref().try_into().unwrap();
        assert!(install_to(&download, &installed, &root.join("work"), &other, &|_| {}).is_err());
        assert_eq!(std::fs::read(&installed).unwrap(), b"old build");
        let _ = std::fs::remove_dir_all(&root);
    }

    // run by hand with MUX_SIGNED_FILE set to a file signed the way ci signs, to check the two agree
    #[test]
    #[ignore]
    fn ci_signatures_pass_the_built_in_key() {
        let file = std::env::var("MUX_SIGNED_FILE").expect("MUX_SIGNED_FILE");
        let bytes = std::fs::read(&file).unwrap();
        let signature = std::fs::read(format!("{file}.sig")).unwrap();
        verify(&bytes, &signature, &PUBLIC_KEY).unwrap();
    }

    #[test]
    fn the_old_copy_sits_hidden_beside_it() {
        assert_eq!(
            old(Path::new("/home/me/.local/bin/mux")),
            Path::new("/home/me/.local/bin/.mux.old")
        );
        assert_eq!(
            old(Path::new("/Applications/Mux.app")),
            Path::new("/Applications/.Mux.app.old")
        );
    }
}
