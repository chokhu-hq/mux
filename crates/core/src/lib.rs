use serde::{Deserialize, Serialize};

pub mod logging;
pub mod paths;
pub mod protocol;
pub mod transport;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn home_dir() -> std::path::PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(Into::into)
        .unwrap_or_default()
}

// tells builds apart: the commit a release was made from, or when a local build's binary was written,
// read once so a binary replaced while running still reports the build that started
pub fn build_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        if let Some(commit) = option_env!("MUX_COMMIT") {
            return commit.to_owned();
        }
        let written = std::env::current_exe()
            .and_then(|exe| exe.metadata())
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok());
        match written {
            Some(time) => format!("local-{}", time.as_millis()),
            None => "local".to_owned(),
        }
    })
}

// windows' build number, where 22000 and up is windows 11, read once from `ver`,
// which prints "Microsoft Windows [Version 10.0.22631.4602]"
#[cfg(windows)]
pub fn windows_build() -> Option<u32> {
    static BUILD: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    *BUILD.get_or_init(|| {
        let output = command("cmd").args(["/C", "ver"]).output().ok()?;
        let text = String::from_utf8_lossy(&output.stdout);
        let version = text.split("Version ").nth(1)?;
        version
            .split('.')
            .nth(2)?
            .trim_end_matches(']')
            .trim()
            .parse()
            .ok()
    })
}

// the shell in the user's account is the one a terminal opens, and SHELL can be stale or missing
#[cfg(unix)]
pub fn login_shell() -> String {
    // getpwuid reads the account record and hands back a pointer into libc's own buffer, copied out at once
    let from_account = unsafe {
        let entry = libc::getpwuid(libc::getuid());
        (!entry.is_null() && !(*entry).pw_shell.is_null()).then(|| {
            std::ffi::CStr::from_ptr((*entry).pw_shell)
                .to_string_lossy()
                .into_owned()
        })
    };
    from_account
        .filter(|shell| !shell.is_empty())
        .or_else(|| std::env::var("SHELL").ok())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

// a program to run, found the way a shell would and kept from opening a console window on windows
pub fn command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut command = std::process::Command::new(resolve(program.as_ref()));
        command.creation_flags(CREATE_NO_WINDOW);
        command
    }
    #[cfg(not(windows))]
    std::process::Command::new(program)
}

// windows only looks for .exe on PATH, but npm installs clis like codex as .cmd scripts
#[cfg(windows)]
fn resolve(program: &std::ffi::OsStr) -> std::ffi::OsString {
    let path = std::path::Path::new(program);
    if path.extension().is_some() || path.components().count() > 1 {
        return program.to_owned();
    }
    let dirs = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&dirs) {
        for ext in ["exe", "cmd", "bat"] {
            let candidate = dir.join(path).with_extension(ext);
            if candidate.is_file() {
                return candidate.into_os_string();
            }
        }
    }
    program.to_owned()
}

// shortens the home folder to ~ the way shells show paths
pub fn display_path(path: &str) -> String {
    let home = home_dir().to_string_lossy().into_owned();
    match path.strip_prefix(&home) {
        Some(rest)
            if !home.is_empty()
                && (rest.is_empty() || rest.starts_with(std::path::MAIN_SEPARATOR)) =>
        {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionKind {
    Agent,
    Shell,
    Task,
}

impl SessionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Shell => "Shell",
            Self::Task => "Task",
        }
    }
}

// which coding agent cli runs an agent session
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    Codex,
    Claude,
    OpenCode,
}

impl Provider {
    pub const ALL: [Provider; 3] = [Provider::Codex, Provider::Claude, Provider::OpenCode];

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::OpenCode => "OpenCode",
        }
    }
}

// how far an agent may go before it stops to ask, the same four modes t3 code offers
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Access {
    Supervised,
    AutoEdits,
    #[default]
    Auto,
    Full,
}

impl Access {
    pub const ALL: [Access; 4] = [
        Access::Supervised,
        Access::AutoEdits,
        Access::Auto,
        Access::Full,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Supervised => "Supervised",
            Self::AutoEdits => "Auto-accept edits",
            Self::Auto => "Auto",
            Self::Full => "Full access",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Supervised => "Ask before commands and file changes",
            Self::AutoEdits => "Edit the project freely, ask before other actions",
            Self::Auto => "The agent reviews routine actions itself",
            Self::Full => "Run commands and edit without asking",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SessionStatus {
    Running,
    Waiting,
    Idle,
    Done,
    Failed,
}

impl SessionStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::Waiting => "Waiting",
            Self::Idle => "Idle",
            Self::Done => "Done",
            Self::Failed => "Failed",
        }
    }
}
