use serde::{Deserialize, Serialize};

pub mod protocol;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

// shortens the home folder to ~ the way shells show paths
pub fn display_path(path: &str) -> String {
    let home =
        std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).unwrap_or_default();
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
    #[default]
    AutoEdits,
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
