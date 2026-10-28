// the server can run inside wsl while this app runs on windows, as vs code's remote does: the app
// installs a matching linux server there, starts it, and reaches it over a port with the secret it wrote
use std::io::{self, Write};
use std::net::TcpStream;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use mux_core::protocol;

// the first start may download the server, so it gets longer than a local one
const START_TIMEOUT: Duration = Duration::from_secs(90);

// the distributions installed, which `wsl -l -q` lists in utf-16
pub fn distros() -> Vec<String> {
    let Ok(output) = mux_core::command("wsl.exe").args(["-l", "-q"]).output() else {
        return Vec::new();
    };
    decode(&output.stdout)
        .lines()
        .map(|line| line.trim().trim_matches('\0').to_owned())
        .filter(|line| !line.is_empty())
        .collect()
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
        .args(["-d", distro, "--", "sh", "-c", &script])
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
        .args(["-d", distro, "--", "sh", "-c", script])
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
        .args(["-d", distro, "--cd", "~", "--", "sh", "-c", &script])
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
