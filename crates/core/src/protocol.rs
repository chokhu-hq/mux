use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{Access, Provider, SessionKind, SessionStatus};

pub const MAX_FRAME: usize = 16 * 1024 * 1024;
// bump on any message change, so a newer app can tell it is talking to an older server
pub const PROTOCOL: u32 = 8;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u64,
    pub title: String,
    pub kind: SessionKind,
    pub status: SessionStatus,
    pub meta: String,
    // unix seconds of the last status change, for the sidebar's relative times
    #[serde(default)]
    pub updated: u64,
    #[serde(default)]
    pub provider: Provider,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupInfo {
    pub id: u64,
    pub name: String,
    // the project folder sessions start in, the home folder when unset
    #[serde(default)]
    pub path: Option<String>,
    // the checked out branch of the folder, filled in by the server when it sends state
    #[serde(default)]
    pub branch: Option<String>,
    pub open: bool,
    pub sessions: Vec<SessionInfo>,
}

// one entry in an agent conversation, shared by every agent backend
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum AgentItem {
    User {
        text: String,
    },
    Assistant {
        text: String,
    },
    Command {
        command: String,
        output: String,
        exit_code: Option<i32>,
        status: ItemStatus,
    },
    FileChange {
        changes: Vec<FileChange>,
        status: ItemStatus,
    },
    Approval {
        id: u64,
        title: String,
        detail: String,
        decision: Option<Decision>,
    },
    Error {
        message: String,
    },
    // any other tool call, like reading a file or searching
    Tool {
        name: String,
        detail: String,
        output: String,
        status: ItemStatus,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub diff: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ItemStatus {
    Running,
    Done,
    Failed,
    Declined,
}

// unset fields mean the agent's own default, so a user's codex config still applies
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettings {
    #[serde(default)]
    pub provider: Provider,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    #[serde(default)]
    pub access: Access,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub name: String,
    pub description: String,
    pub efforts: Vec<String>,
    pub default_effort: String,
    pub is_default: bool,
    // the description of the model's fast tier, when it has one
    pub fast: Option<String>,
}

// one rate limit window of an agent's plan, like its five hour or weekly allowance
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    pub label: String,
    // how much of the window is used, from 0 to 1
    pub used: f32,
    // unix seconds when the window resets, 0 when unknown
    pub resets_at: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Decision {
    Allow,
    AlwaysAllow,
    Deny,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
// hello, shutdown and welcome keep their spots so any two versions can still greet and stop each other
pub enum ClientMsg {
    Hello {
        protocol: u32,
    },
    Shutdown,
    CreateGroup {
        name: String,
        path: Option<String>,
    },
    ToggleGroup {
        id: u64,
    },
    RenameGroup {
        id: u64,
        name: String,
    },
    SetGroupPath {
        id: u64,
        path: Option<String>,
    },
    DeleteGroup {
        id: u64,
    },
    CreateSession {
        group: Option<u64>,
        title: String,
        kind: SessionKind,
    },
    DeleteSession {
        id: u64,
    },
    MoveSession {
        id: u64,
        group: u64,
        index: usize,
    },
    Attach {
        id: u64,
    },
    Detach {
        id: u64,
    },
    Restart {
        id: u64,
    },
    Input {
        id: u64,
        data: Vec<u8>,
    },
    Resize {
        id: u64,
        cols: u16,
        rows: u16,
    },
    // subscribes to an agent session and asks for its transcript so far
    AgentOpen {
        id: u64,
    },
    AgentSend {
        id: u64,
        text: String,
    },
    AgentDecide {
        id: u64,
        approval: u64,
        decision: Decision,
    },
    AgentStop {
        id: u64,
    },
    AgentConfigure {
        id: u64,
        settings: AgentSettings,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        protocol: u32,
    },
    State {
        groups: Vec<GroupInfo>,
    },
    Created {
        id: u64,
    },
    Attached {
        id: u64,
        cols: u16,
        rows: u16,
        replay: Vec<u8>,
    },
    Output {
        id: u64,
        data: Vec<u8>,
    },
    AgentTranscript {
        id: u64,
        items: Vec<AgentItem>,
        busy: bool,
        settings: AgentSettings,
    },
    // sets the item at `index`, appending when it is one past the end
    AgentItem {
        id: u64,
        index: usize,
        item: AgentItem,
    },
    AgentBusy {
        id: u64,
        busy: bool,
    },
    AgentSettings {
        id: u64,
        settings: AgentSettings,
    },
    AgentModels {
        provider: Provider,
        models: Vec<ModelOption>,
    },
    AgentUsage {
        provider: Provider,
        windows: Vec<UsageWindow>,
    },
}

// frames are a little-endian u32 length followed by a postcard body
pub fn encode<T: Serialize>(msg: &T) -> io::Result<Vec<u8>> {
    let body = postcard::to_allocvec(msg).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

pub fn decode<T: DeserializeOwned>(body: &[u8]) -> io::Result<T> {
    postcard::from_bytes(body).map_err(io::Error::other)
}

pub fn frame_len(header: [u8; 4]) -> io::Result<usize> {
    let len = u32::from_le_bytes(header) as usize;
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame of {len} bytes is over the limit"),
        ));
    }
    Ok(len)
}

pub fn read<T: DeserializeOwned>(reader: &mut impl Read) -> io::Result<T> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let mut body = vec![0; frame_len(header)?];
    reader.read_exact(&mut body)?;
    decode(&body)
}

pub fn write<T: Serialize>(writer: &mut impl Write, msg: &T) -> io::Result<()> {
    writer.write_all(&encode(msg)?)?;
    writer.flush()
}

pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MUX_SOCKET") {
        return path.into();
    }
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("mux.sock"),
        None => std::env::temp_dir().join(format!("mux-{}.sock", user())),
    }
}

pub fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MUX_DATA_DIR") {
        return dir.into();
    }
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/mux")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("mux")
    }
}

fn user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "user".to_owned())
}
