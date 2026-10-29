// runs the real server in a folder of its own, with the fake claude cli first on its PATH, and talks to it
// the way the app does, so tests cover mux's own code from the socket to the agent process
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use mux_core::protocol::{self, AgentItem, AgentSettings, ClientMsg, PROTOCOL, ServerMsg};
use mux_core::transport::{self, Stream};
use mux_core::{Provider, SessionKind};
use serde_json::Value;

const PATIENCE: Duration = Duration::from_secs(15);

pub struct Server {
    pub dir: PathBuf,
    pub socket: PathBuf,
    child: Child,
    stream: Stream,
    inbox: Receiver<ServerMsg>,
    group: Option<u64>,
    // the agent made last, whose transcript `items` and `busy` follow
    focus: Option<u64>,
    pub items: Vec<AgentItem>,
    pub busy: bool,
    // every opened agent's transcript and whether it's working
    pub agents: HashMap<u64, (Vec<AgentItem>, bool)>,
    pub seen: Vec<ServerMsg>,
}

impl Server {
    pub fn start() -> Self {
        Self::start_with(&[])
    }

    // `env` is added to the server's environment, which the agents it starts inherit
    pub fn start_with(env: &[(&str, &str)]) -> Self {
        Self::start_limited(env, None)
    }

    // like start_with, with the server allowed only this many open files, so leaks show up as failures
    pub fn start_limited(env: &[(&str, &str)], open_files: Option<u64>) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        // cargo's scratch folder for tests, under target, so a failed run's folder is there to look at
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "mux-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let bin = dir.join("bin");
        for folder in [&bin, &dir.join("home"), &dir.join("project")] {
            std::fs::create_dir_all(folder).unwrap();
        }
        install_fake(&bin);
        let socket = dir.join("mux.sock");
        let mut command = Command::new(env!("CARGO_BIN_EXE_mux-server"));
        // only the fake is on PATH, so no real agent cli ever runs
        command
            .env_clear()
            .env("PATH", &bin)
            .env("HOME", dir.join("home"))
            .env("MUX_SOCKET", &socket)
            .env("MUX_DATA_DIR", dir.join("data"))
            .env("FAKE_CLAUDE_LOG", dir.join("claude.log"));
        for (key, value) in env {
            command.env(key, value);
        }
        #[cfg(unix)]
        if let Some(limit) = open_files {
            use std::os::unix::process::CommandExt;
            let limit = libc::rlimit {
                rlim_cur: limit as libc::rlim_t,
                rlim_max: limit as libc::rlim_t,
            };
            // runs in the child between fork and exec, and only makes one system call
            unsafe {
                command.pre_exec(move || {
                    if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        #[cfg(not(unix))]
        let _ = open_files;
        let child = command.spawn().expect("the server starts");
        let stream = connect(&socket);
        let reader = stream.try_clone().unwrap();
        let (tx, inbox) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = reader;
            while let Ok(msg) = protocol::read::<ServerMsg>(&mut reader) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
        let mut server = Server {
            dir,
            socket,
            child,
            stream,
            inbox,
            group: None,
            focus: None,
            items: Vec::new(),
            busy: false,
            agents: HashMap::new(),
            seen: Vec::new(),
        };
        server.send(ClientMsg::Hello { protocol: PROTOCOL });
        server.wait("the welcome", |s| {
            s.seen
                .iter()
                .any(|m| matches!(m, ServerMsg::Welcome { .. }))
        });
        server
    }

    pub fn send(&mut self, msg: ClientMsg) {
        protocol::write(&mut self.stream, &msg).unwrap();
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    // another client on the same server, already past the hello, which reads nothing unless asked to
    pub fn client(&self) -> Stream {
        let mut stream = connect(&self.socket);
        protocol::write(&mut stream, &ClientMsg::Hello { protocol: PROTOCOL }).unwrap();
        stream
    }

    // a claude agent in its own project folder, opened so its updates come here
    pub fn claude_agent(&mut self, settings: AgentSettings) -> u64 {
        let group = match self.group {
            Some(group) => group,
            None => {
                let path = self.dir.join("project").to_string_lossy().into_owned();
                self.send(ClientMsg::CreateGroup {
                    name: "project".into(),
                    path: Some(path),
                });
                let group = self.wait_for("the project", |msg| match msg {
                    ServerMsg::State { groups } => groups.first().map(|g| g.id),
                    _ => None,
                });
                self.group = Some(group);
                group
            }
        };
        self.send(ClientMsg::CreateSession {
            group: Some(group),
            title: "New agent".into(),
            kind: SessionKind::Agent,
        });
        let id = self.wait_for("the session", |msg| match msg {
            ServerMsg::Created { id } => Some(*id),
            _ => None,
        });
        self.send(ClientMsg::AgentConfigure {
            id,
            settings: AgentSettings {
                provider: Provider::Claude,
                ..settings
            },
        });
        self.send(ClientMsg::AgentOpen { id });
        self.focus = Some(id);
        self.items.clear();
        self.busy = false;
        id
    }

    pub fn say(&mut self, id: u64, text: &str) {
        self.send(ClientMsg::AgentSend {
            id,
            text: text.into(),
            attachments: Vec::new(),
        });
    }

    // takes in messages until `done` holds, keeping the agent's transcript up to date on the way
    pub fn wait(&mut self, what: &str, done: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        while !done(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.inbox.recv_timeout(left) {
                Ok(msg) => self.take(msg),
                Err(_) => panic!(
                    "gave up waiting for {what}\ntranscript: {:#?}\nfake claude saw: {:#?}",
                    self.items,
                    self.claude_log()
                ),
            }
        }
    }

    // takes in messages until one gives a value
    pub fn wait_for<T>(&mut self, what: &str, pick: impl Fn(&ServerMsg) -> Option<T>) -> T {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let Ok(msg) = self.inbox.recv_timeout(left) else {
                panic!("gave up waiting for {what}, after {:#?}", self.seen);
            };
            let picked = pick(&msg);
            self.take(msg);
            if let Some(value) = picked {
                return value;
            }
        }
    }

    pub fn wait_idle(&mut self) {
        self.wait("the turn to end", |s| {
            !s.busy
                && s.items
                    .iter()
                    .any(|i| matches!(i, AgentItem::Assistant { .. }))
        });
    }

    fn take(&mut self, msg: ServerMsg) {
        let id = match &msg {
            ServerMsg::AgentTranscript {
                id, items, busy, ..
            } => {
                self.agents.insert(*id, (items.clone(), *busy));
                Some(*id)
            }
            ServerMsg::AgentItem { id, index, item } => {
                let (items, _) = self.agents.entry(*id).or_default();
                if *index < items.len() {
                    items[*index] = item.clone();
                } else {
                    items.push(item.clone());
                }
                Some(*id)
            }
            ServerMsg::AgentBusy { id, busy } => {
                self.agents.entry(*id).or_default().1 = *busy;
                Some(*id)
            }
            _ => None,
        };
        if id.is_some()
            && id == self.focus
            && let Some((items, busy)) = self.agents.get(&id.unwrap())
        {
            self.items = items.clone();
            self.busy = *busy;
        }
        // the stress test sends a lot, and only the agent updates are needed from it
        if id.is_none() {
            self.seen.push(msg);
        }
    }

    pub fn assistant_text(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| match i {
                AgentItem::Assistant { text } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    pub fn errors(&self) -> Vec<String> {
        self.items
            .iter()
            .filter_map(|i| match i {
                AgentItem::Error { message } => Some(message.clone()),
                _ => None,
            })
            .collect()
    }

    // every line the fake claude wrote down: how each one was started, and all it was sent
    pub fn claude_log(&self) -> Vec<Value> {
        read_log(&self.dir.join("claude.log"))
    }

    // the json mux wrote to the agent processes' stdin, in order
    pub fn claude_stdin(&self) -> Vec<Value> {
        let agents: Vec<Value> = self
            .claude_launches()
            .iter()
            .map(|l| l["pid"].clone())
            .collect();
        self.claude_log()
            .iter()
            .filter(|e| agents.contains(&e["pid"]))
            .filter_map(|e| e["stdin"].as_str())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    // how each agent process was started, leaving out the short-lived one that lists models
    pub fn claude_launches(&self) -> Vec<Value> {
        self.claude_log()
            .into_iter()
            .filter(|e| {
                e["argv"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|v| v == "--permission-prompt-tool"))
            })
            .collect()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = protocol::write(&mut self.stream, &ClientMsg::Shutdown);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
}

fn install_fake(bin: &Path) {
    let fake = PathBuf::from(env!("CARGO_BIN_EXE_fake-claude"));
    #[cfg(unix)]
    std::os::unix::fs::symlink(&fake, bin.join("claude")).unwrap();
    #[cfg(windows)]
    std::fs::copy(&fake, bin.join("claude.exe")).unwrap();
}

fn connect(socket: &Path) -> Stream {
    let deadline = Instant::now() + PATIENCE;
    loop {
        match transport::connect(socket) {
            Ok(stream) => return stream,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(err) => panic!("the server never listened: {err}"),
        }
    }
}

fn read_log(path: &Path) -> Vec<Value> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str(&line).ok())
        .collect()
}
