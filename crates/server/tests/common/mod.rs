// runs the real server in a folder of its own, with the fake claude cli first on its PATH, and talks to it
// the way the app does, so tests cover mux's own code from the socket to the agent process
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
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
    env: Vec<(String, String)>,
    open_files: Option<u64>,
    live: bool,
    patience: Duration,
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
    // how many bytes the server has sent this client
    pub received: Arc<AtomicUsize>,
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
        Self::start_in(env, open_files, false)
    }

    // the real agent clis on this machine, signed in as the user, for the few checks a fake can't answer.
    // they cost real tokens, so the tests using this are ignored unless asked for
    pub fn start_live() -> Self {
        Self::start_in(&[], None, true)
    }

    fn start_in(env: &[(&str, &str)], open_files: Option<u64>, live: bool) -> Self {
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
        if !live {
            install_fake(&bin);
        }
        let env: Vec<(String, String)> = env
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let socket = dir.join("mux.sock");
        let child = launch(&dir, &env, open_files, live);
        let (stream, inbox, received) = listen(&socket);
        let mut server = Server {
            dir,
            socket,
            env,
            open_files,
            live,
            patience: if live {
                Duration::from_secs(120)
            } else {
                PATIENCE
            },
            child,
            stream,
            inbox,
            group: None,
            focus: None,
            items: Vec::new(),
            busy: false,
            agents: HashMap::new(),
            seen: Vec::new(),
            received,
        };
        server.hello();
        server
    }

    fn hello(&mut self) {
        self.send(ClientMsg::Hello { protocol: PROTOCOL });
        self.wait("the welcome", |s| {
            s.seen
                .iter()
                .any(|m| matches!(m, ServerMsg::Welcome { .. }))
        });
    }

    // kills the server the way a crash would and starts a new one on the same data, opening the same agent again
    pub fn crash_and_restart(&mut self) {
        self.crash();
        self.relaunch();
    }

    // quits the server the way the tray's quit does, then starts it again
    pub fn quit_and_restart(&mut self) {
        self.send(ClientMsg::Shutdown);
        let deadline = Instant::now() + PATIENCE;
        while self.child.try_wait().unwrap().is_none() {
            assert!(Instant::now() < deadline, "the server never quit");
            std::thread::sleep(Duration::from_millis(20));
        }
        self.relaunch();
    }

    fn relaunch(&mut self) {
        self.child = launch(&self.dir, &self.env, self.open_files, self.live);
        (self.stream, self.inbox, self.received) = listen(&self.socket);
        self.seen.clear();
        self.agents.clear();
        self.items.clear();
        self.busy = false;
        self.hello();
        if let Some(id) = self.focus {
            self.send(ClientMsg::AgentOpen { id });
        }
    }

    pub fn send(&mut self, msg: ClientMsg) {
        protocol::write(&mut self.stream, &msg).unwrap();
    }

    // kills the server outright, the way a crash or a force quit would
    pub fn crash(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
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
        self.agent(Provider::Claude, settings)
    }

    pub fn codex_agent(&mut self, settings: AgentSettings) -> u64 {
        self.agent(Provider::Codex, settings)
    }

    fn agent(&mut self, provider: Provider, settings: AgentSettings) -> u64 {
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
                provider,
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
        let deadline = Instant::now() + self.patience;
        while !done(self) {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.inbox.recv_timeout(left) {
                Ok(msg) => self.take(msg),
                Err(_) => panic!(
                    "gave up waiting for {what}\ntranscript: {:#?}\nfake claude saw: {:#?}\nfake codex saw: {:#?}",
                    self.items,
                    self.claude_log(),
                    self.codex_stdin()
                ),
            }
        }
    }

    // takes in messages until one gives a value
    pub fn wait_for<T>(&mut self, what: &str, pick: impl Fn(&ServerMsg) -> Option<T>) -> T {
        let deadline = Instant::now() + self.patience;
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
            ServerMsg::AgentAppend {
                id,
                index,
                at,
                text,
            } => {
                let (items, _) = self.agents.entry(*id).or_default();
                let current = items
                    .get_mut(*index)
                    .and_then(AgentItem::streamed_text)
                    .expect("streamed text goes to an item the client has");
                assert_eq!(current.len(), *at, "streamed text lands where it was cut");
                current.push_str(text);
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

    // every line the fake codex wrote down, from every codex mux started
    pub fn codex_log(&self) -> Vec<Value> {
        read_log(&self.dir.join("codex.log"))
    }

    // the json-rpc mux sent the codexes that ran conversations, leaving out the ones that only listed things
    pub fn codex_stdin(&self) -> Vec<Value> {
        let lines: Vec<(Value, Value)> = self
            .codex_log()
            .into_iter()
            .filter_map(|e| {
                let msg = serde_json::from_str(e["stdin"].as_str()?).ok()?;
                Some((e["pid"].clone(), msg))
            })
            .collect();
        let agents: Vec<&Value> = lines
            .iter()
            .filter(|(_, m)| {
                m["method"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("thread/"))
            })
            .map(|(pid, _)| pid)
            .collect();
        lines
            .iter()
            .filter(|(pid, _)| agents.contains(&pid))
            .map(|(_, m)| m.clone())
            .collect()
    }

    // the calls mux made of the conversation codexes, by method, in order
    pub fn codex_calls(&self, method: &str) -> Vec<Value> {
        self.codex_stdin()
            .into_iter()
            .filter(|m| m["method"] == method)
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

fn launch(dir: &Path, env: &[(String, String)], open_files: Option<u64>, live: bool) -> Child {
    let mut command = Command::new(env!("CARGO_BIN_EXE_mux-server"));
    command.env_clear();
    if live {
        // the real clis, with the user's own sign-in
        for key in ["PATH", "HOME", "USER", "LOGNAME", "LANG"] {
            if let Some(value) = std::env::var_os(key) {
                command.env(key, value);
            }
        }
    } else {
        // only the fake is on PATH, so no real agent cli ever runs
        command
            .env("PATH", dir.join("bin"))
            .env("HOME", dir.join("home"));
    }
    command
        .env("MUX_SOCKET", dir.join("mux.sock"))
        .env("MUX_DATA_DIR", dir.join("data"))
        .env("FAKE_CLAUDE_LOG", dir.join("claude.log"))
        .env("FAKE_CODEX_LOG", dir.join("codex.log"));
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
    command.spawn().expect("the server starts")
}

// connects to the server and reads what it sends on a thread of its own, counting the bytes
fn listen(socket: &Path) -> (Stream, Receiver<ServerMsg>, Arc<AtomicUsize>) {
    let stream = connect(socket);
    let reader = stream.try_clone().unwrap();
    let (tx, inbox) = mpsc::channel();
    let received = Arc::new(AtomicUsize::new(0));
    let counted = received.clone();
    std::thread::spawn(move || {
        let mut reader = reader;
        while let Ok(msg) = protocol::read::<ServerMsg>(&mut reader) {
            let size = protocol::encode(&msg).map_or(0, |frame| frame.len());
            counted.fetch_add(size, Ordering::Relaxed);
            if tx.send(msg).is_err() {
                break;
            }
        }
    });
    (stream, inbox, received)
}

fn install_fake(bin: &Path) {
    for (name, fake) in [
        ("claude", env!("CARGO_BIN_EXE_fake-claude")),
        ("codex", env!("CARGO_BIN_EXE_fake-codex")),
    ] {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fake, bin.join(name)).unwrap();
        #[cfg(windows)]
        std::fs::copy(fake, bin.join(name).with_extension("exe")).unwrap();
    }
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
