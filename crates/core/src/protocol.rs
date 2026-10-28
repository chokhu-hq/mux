use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::{SessionKind, SessionStatus};

pub const MAX_FRAME: usize = 16 * 1024 * 1024;
// bump on any message change, so a newer app can tell it is talking to an older server
pub const PROTOCOL: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: u64,
    pub title: String,
    pub kind: SessionKind,
    pub status: SessionStatus,
    pub meta: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GroupInfo {
    pub id: u64,
    pub name: String,
    // the project folder sessions start in, the home folder when unset
    #[serde(default)]
    pub path: Option<String>,
    pub open: bool,
    pub sessions: Vec<SessionInfo>,
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
