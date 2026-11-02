// the server can run inside wsl while this app runs on windows, as vs code's remote does: the app
// installs a matching linux server there, starts it, and reaches it over a port with the secret it wrote
// every call uses --exec, because `--` hands the line to the default shell, which expands the script's variables first
use std::io::{self, Write};
use std::net::TcpStream;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use mux_core::protocol;

// the first start may download the server, so it gets longer than a local one
const START_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Default)]
pub struct Distros {
    pending: Option<std::sync::mpsc::Receiver<Vec<String>>>,
    ready: Option<Vec<String>>,
}

impl Distros {
    pub fn poll(&mut self, ctx: &eframe::egui::Context) -> Option<Vec<String>> {
        if self.pending.is_none() && self.ready.is_none() {
            let (tx, rx) = std::sync::mpsc::channel();
            self.pending = Some(rx);
            let ctx = ctx.clone();
            thread::spawn(move || {
                let _ = tx.send(distros());
                ctx.request_repaint();
            });
        }
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Ok(distros) => self.ready = Some(distros),
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.ready = Some(Vec::new());
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.ready.is_some() {
            self.pending = None;
        }
        self.ready.clone()
    }
}

// WSL can stall even when only listing distributions, so never wait on the UI thread.
fn distros() -> Vec<String> {
    let mut command = mux_core::command("wsl.exe");
    command.args(["-l", "-q"]);
    match output_with_timeout(&mut command, Duration::from_secs(5)) {
        Ok(output) if output.status.success() => decode(&output.stdout)
            .lines()
            .map(|line| line.trim().trim_matches('\0').to_owned())
            .filter(|line| !line.is_empty())
            .collect(),
        Ok(output) => {
            log::warn!("couldn't list WSL distributions: {}", output.status);
            Vec::new()
        }
        Err(error) => {
            log::warn!("couldn't list WSL distributions: {error}");
            Vec::new()
        }
    }
}

fn output_with_timeout(
    command: &mut std::process::Command,
    timeout: Duration,
) -> io::Result<std::process::Output> {
    use std::io::Read;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout.read_to_end(&mut bytes).map(|_| bytes);
        let _ = tx.send(result);
    });
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = rx
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "WSL output timed out")
                    })??;
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr: Vec::new(),
                });
            }
            Ok(None) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(20));
            }
            result => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(match result {
                    Err(error) => error,
                    _ => io::Error::new(io::ErrorKind::TimedOut, "WSL discovery timed out"),
                });
            }
        }
    }
}

fn decode(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .filter(|&unit| unit != 0xfeff)
        .collect();
    String::from_utf16_lossy(&units)
}

pub fn connect(distro: &str) -> io::Result<TcpStream> {
    if let Ok(stream) = reach(distro) {
        return Ok(stream);
    }
    log::info!("starting a server in wsl ({distro})");
    let mut child = start(distro)?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        if let Ok(stream) = reach(distro) {
            return Ok(stream);
        }
        // a server that couldn't be installed or started says why in its output
        let gone = child.try_wait().ok().flatten().is_some();
        if gone || Instant::now() > deadline {
            let output = std::fs::read_to_string(output_log()).unwrap_or_default();
            let reason = output.lines().rev().find(|l| !l.trim().is_empty());
            log::error!("the server in wsl ({distro}) didn't start: {output}");
            return Err(io::Error::other(match reason {
                Some(reason) => format!("The server in WSL ({distro}) didn't start: {reason}"),
                None => format!("The server in WSL ({distro}) didn't answer"),
            }));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

// which of these programs the login shell in wsl can find, as the server will when it starts agents
pub fn installed<'a>(distro: &str, programs: &[&'a str]) -> Vec<&'a str> {
    let names = programs.join(" ");
    let script = format!(
        r#"case "${{SHELL##*/}}" in
  fish) exec "$SHELL" -l -i -c 'for c in {names}; command -v $c; end' ;;
  *) exec "${{SHELL:-/bin/sh}}" -l -i -c 'for c in {names}; do command -v "$c"; done' ;;
esac"#
    );
    let Ok(output) = mux_core::command("wsl.exe")
        .args(["-d", distro, "--exec", "sh", "-c", &script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    // an interactive shell may print a greeting too, so only lines naming a program's path count
    programs
        .iter()
        .copied()
        .filter(|program| {
            text.lines()
                .any(|line| line.trim().ends_with(&format!("/{program}")))
        })
        .collect()
}

// reads where the server listens from inside wsl, and remembers its data folder for the logs tab
fn reach(distro: &str) -> io::Result<TcpStream> {
    let script = r#"d="${XDG_DATA_HOME:-$HOME/.local/share}/mux"; printf '%s\n' "$d"; cat "$d/server.address""#;
    let output = mux_core::command("wsl.exe")
        .args(["-d", distro, "--exec", "sh", "-c", script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let (folder, address) = text.split_once('\n').unwrap_or((&text, ""));
    let (port, secret) = mux_core::transport::parse_address(address)?;
    let mut stream =
        TcpStream::connect_timeout(&([127, 0, 0, 1], port).into(), Duration::from_secs(2))?;
    stream.write_all(format!("{secret}\n").as_bytes())?;
    set_data_dir(folder.trim());
    Ok(stream)
}

// installs this build's server into ~/.mux/bin when it isn't there, then runs it through the login shell
// so the agent clis are on its PATH the way they are in a terminal
fn start(distro: &str) -> io::Result<std::process::Child> {
    let (name, download) = match crate::update::server_release() {
        Some((tag, base)) => (format!("mux-server-{tag}"), base),
        // a build from source has no release to fetch, so it uses a server put there by hand
        None => ("mux-server".to_owned(), String::new()),
    };
    let script = format!(
        r#"set -e
dir="$HOME/.mux/bin"
bin="$dir/{name}"
if [ ! -x "$bin" ]; then
  if [ -z "{download}" ]; then
    echo "Build mux-server in WSL and copy it to $bin"
    exit 1
  fi
  mkdir -p "$dir"
  url="{download}/mux-server-linux-$(uname -m)"
  echo "Downloading $url"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$bin.part"
  else
    wget -qO "$bin.part" "$url"
  fi
  chmod +x "$bin.part"
  mv "$bin.part" "$bin"
fi
export MUX_LISTEN=1
case "${{SHELL##*/}}" in
  fish) exec "$SHELL" -l -i -c 'exec $argv' "$bin" ;;
  *) exec "${{SHELL:-/bin/sh}}" -l -i -c 'exec "$0"' "$bin" ;;
esac
"#
    );
    let log = output_log();
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let output = std::fs::File::create(&log)?;
    mux_core::command("wsl.exe")
        .args(["-d", distro, "--cd", "~", "--exec", "sh", "-c", &script])
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .spawn()
}

fn output_log() -> std::path::PathBuf {
    protocol::data_dir().join("wsl-server-output.log")
}

static DATA_DIR: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

fn set_data_dir(folder: &str) {
    if let Ok(mut dir) = DATA_DIR.write() {
        *dir = Some(folder.to_owned()).filter(|f| !f.is_empty());
    }
}

// the server's data folder inside wsl, as linux names it
pub fn data_dir() -> Option<String> {
    DATA_DIR.read().ok().and_then(|dir| dir.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_poll_does_not_wait_for_worker() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut distros = Distros {
            pending: Some(rx),
            ready: None,
        };
        let ctx = eframe::egui::Context::default();
        let started = Instant::now();
        assert_eq!(distros.poll(&ctx), None);
        assert!(started.elapsed() < Duration::from_millis(100));
        tx.send(vec!["Ubuntu".to_owned()]).unwrap();
        assert_eq!(distros.poll(&ctx), Some(vec!["Ubuntu".to_owned()]));
        assert!(distros.pending.is_none());
        assert_eq!(distros.poll(&ctx), Some(vec!["Ubuntu".to_owned()]));
    }

    #[test]
    fn disconnected_discovery_is_cached() {
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx);
        let mut distros = Distros {
            pending: Some(rx),
            ready: None,
        };
        assert_eq!(
            distros.poll(&eframe::egui::Context::default()),
            Some(Vec::new())
        );
        assert!(distros.pending.is_none());
    }

    #[test]
    fn stalled_discovery_process_times_out() {
        let mut command = mux_core::command(std::env::current_exe().unwrap());
        command.args(["--exact", "wsl::tests::discovery_child", "--ignored"]);
        command.env("MUX_TEST_DISCOVERY_CHILD", "sleep");
        let started = Instant::now();
        let error = output_with_timeout(&mut command, Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn discovery_drains_output_while_child_runs() {
        let mut command = mux_core::command(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "wsl::tests::discovery_child",
            "--ignored",
            "--nocapture",
        ]);
        command.env("MUX_TEST_DISCOVERY_CHILD", "output");
        let output = output_with_timeout(&mut command, Duration::from_secs(5)).unwrap();
        assert!(output.status.success());
        assert!(output.stdout.len() >= 131072);
    }

    #[test]
    #[ignore = "subprocess fixture for WSL discovery tests"]
    fn discovery_child() {
        match std::env::var("MUX_TEST_DISCOVERY_CHILD").as_deref() {
            Ok("sleep") => thread::sleep(Duration::from_secs(30)),
            Ok("output") => std::io::stdout().write_all(&vec![b'x'; 131072]).unwrap(),
            _ => {}
        }
    }

    #[test]
    fn decode_wsl_utf16_output() {
        let bytes: Vec<u8> = "\u{feff}Ubuntu\r\nDebian\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(decode(&bytes), "Ubuntu\r\nDebian\r\n");
    }
}
