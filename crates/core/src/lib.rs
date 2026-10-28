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
