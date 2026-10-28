// an app opened from finder, the dock or a desktop launcher gets a bare PATH without the folders
// a shell profile adds, like the ones npm, nvm or homebrew install the agent clis into
use std::io::Read;
use std::process::Stdio;
use std::time::{Duration, Instant};

const MARK: &str = "__MUX_PATH__";
// a slow shell profile shouldn't hold up the window for long
const TIMEOUT: Duration = Duration::from_secs(5);

// borrows the login shell's PATH when the app wasn't started from a terminal, which already has it
pub fn adopt_login_path() {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return;
    }
    let Some(found) = login_path() else {
        log::warn!("couldn't read PATH from the login shell");
        return;
    };
    let current = std::env::var("PATH").unwrap_or_default();
    let merged = merge(&found, &current);
    if merged != current {
        log::info!("using the login shell's PATH: {merged}");
        // nothing else runs yet, since this is called first thing in main before any thread starts
        unsafe { std::env::set_var("PATH", merged) };
    }
}

fn login_path() -> Option<String> {
    let shell = mux_core::login_shell();
    let script = if shell.ends_with("/fish") {
        format!("printf '{MARK}%s{MARK}' (string join : $PATH)")
    } else {
        format!("printf '{MARK}%s{MARK}' \"$PATH\"")
    };
    let mut child = mux_core::command(&shell)
        .args(["-l", "-i", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut output = String::new();
    child.stdout.take()?.read_to_string(&mut output).ok()?;
    marked(&output)
}

// a profile may print a greeting too, so only what sits between the marks counts
fn marked(output: &str) -> Option<String> {
    let start = output.find(MARK)? + MARK.len();
    let len = output[start..].find(MARK)?;
    Some(output[start..start + len].to_owned()).filter(|path| !path.is_empty())
}

// the shell's folders first, as a terminal would order them, then anything the app had that it didn't
fn merge(shell: &str, current: &str) -> String {
    let mut dirs: Vec<&str> = Vec::new();
    for dir in shell.split(':').chain(current.split(':')) {
        if !dir.is_empty() && !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_path_between_the_marks() {
        let output = format!("Welcome back!\n{MARK}/opt/homebrew/bin:/usr/bin{MARK}");
        assert_eq!(
            marked(&output).as_deref(),
            Some("/opt/homebrew/bin:/usr/bin")
        );
        assert_eq!(marked("no marks here"), None);
        assert_eq!(marked(&format!("{MARK}{MARK}")), None);
    }

    #[test]
    fn puts_the_shells_folders_first_without_repeats() {
        assert_eq!(
            merge("/opt/homebrew/bin:/usr/bin", "/usr/bin:/bin"),
            "/opt/homebrew/bin:/usr/bin:/bin"
        );
    }
}
