mod agent;
mod process;
mod store;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use mux_core::SessionStatus;
use mux_core::protocol::{self, ClientMsg, PROTOCOL, ServerMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use agent::Agent;
use process::{Change, Process};
use store::Store;

pub enum Event {
    Connected(u64, UnboundedSender<ServerMsg>),
    Disconnected(u64),
    Client(u64, ClientMsg),
    Output {
        session: u64,
        generation: u64,
        data: Vec<u8>,
    },
    Exited {
        session: u64,
        generation: u64,
        code: Option<i32>,
    },
    Codex {
        session: u64,
        generation: u64,
        msg: serde_json::Value,
    },
    CodexExited {
        session: u64,
        generation: u64,
    },
}

// blocks until ctrl+c or a client asks the server to shut down
pub fn run() -> io::Result<()> {
    run_at(
        protocol::socket_path(),
        protocol::data_dir().join("state.json"),
    )
}

pub fn run_at(socket: PathBuf, state: PathBuf) -> io::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(serve(socket, state))
}

async fn serve(socket: PathBuf, state: PathBuf) -> io::Result<()> {
    let listener = bind(&socket)?;
    println!(
        "mux server {} listening on {}",
        mux_core::VERSION,
        socket.display()
    );

    let (events, inbox) = mpsc::unbounded_channel();
    let (stop, mut stopped) = mpsc::unbounded_channel::<()>();
    let store = Store::load(&state);
    // write right away so a fresh install has its seeded sessions on disk
    store.save(&state)?;
    let server = Server {
        store,
        state,
        clients: HashMap::new(),
        processes: HashMap::new(),
        agents: HashMap::new(),
        attached: HashMap::new(),
        generation: 0,
        events: events.clone(),
        stop,
    };
    tokio::spawn(server.run(inbox));

    let mut next_client = 0;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                next_client += 1;
                tokio::spawn(connection(stream, next_client, events.clone()));
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = stopped.recv() => break,
        }
    }
    let _ = std::fs::remove_file(&socket);
    Ok(())
}

fn bind(path: &Path) -> io::Result<UnixListener> {
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a mux server is already running on {}", path.display()),
            ));
        }
        // nobody answered, so this is a leftover from a server that did not shut down cleanly
        std::fs::remove_file(path)?;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let listener = UnixListener::bind(path)?;
    use std::os::unix::fs::PermissionsExt;
    // the socket gives full control of the user's shells, so only the owner may connect
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

async fn connection(stream: UnixStream, id: u64, events: UnboundedSender<Event>) {
    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut outbox) = mpsc::unbounded_channel::<ServerMsg>();
    if events.send(Event::Connected(id, tx)).is_err() {
        return;
    }
    let send = tokio::spawn(async move {
        while let Some(msg) = outbox.recv().await {
            let Ok(frame) = protocol::encode(&msg) else {
                continue;
            };
            if writer.write_all(&frame).await.is_err() {
                break;
            }
        }
    });

    loop {
        let mut header = [0; 4];
        if reader.read_exact(&mut header).await.is_err() {
            break;
        }
        let Ok(len) = protocol::frame_len(header) else {
            break;
        };
        let mut body = vec![0; len];
        if reader.read_exact(&mut body).await.is_err() {
            break;
        }
        match protocol::decode::<ClientMsg>(&body) {
            Ok(msg) => {
                if events.send(Event::Client(id, msg)).is_err() {
                    break;
                }
            }
            Err(err) => {
                eprintln!("mux: dropping client {id}, bad message: {err}");
                break;
            }
        }
    }
    let _ = events.send(Event::Disconnected(id));
    send.abort();
}

struct Server {
    store: Store,
    state: PathBuf,
    clients: HashMap<u64, UnboundedSender<ServerMsg>>,
    processes: HashMap<u64, Process>,
    agents: HashMap<u64, Agent>,
    // which clients are watching each session's terminal
    attached: HashMap<u64, HashSet<u64>>,
    // bumped per spawn so output from a killed process can't leak into its replacement
    generation: u64,
    events: UnboundedSender<Event>,
    stop: UnboundedSender<()>,
}

impl Server {
    async fn run(mut self, mut inbox: UnboundedReceiver<Event>) {
        while let Some(event) = inbox.recv().await {
            self.handle(event);
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Connected(client, tx) => {
                let _ = tx.send(ServerMsg::Welcome { protocol: PROTOCOL });
                let _ = tx.send(self.state_msg());
                self.clients.insert(client, tx);
            }
            Event::Disconnected(client) => {
                self.clients.remove(&client);
                for watchers in self.attached.values_mut() {
                    watchers.remove(&client);
                }
            }
            Event::Client(client, msg) => self.message(client, msg),
            Event::Output {
                session,
                generation,
                data,
            } => {
                let Some(process) = self.live(session, generation) else {
                    return;
                };
                let changes = process.feed(&data);
                self.send_watchers(session, ServerMsg::Output { id: session, data });
                for change in changes {
                    let Change::Title(title) = change;
                    if let Some(info) = self.store.session_mut(session)
                        && let Some(title) = title
                        && info.meta != title
                    {
                        info.meta = title;
                        self.broadcast_state();
                    }
                }
            }
            Event::Exited {
                session,
                generation,
                code,
            } => {
                let Some(process) = self.live(session, generation) else {
                    return;
                };
                process.exited = true;
                if let Some(info) = self.store.session_mut(session) {
                    (info.status, info.meta) = match code {
                        Some(0) => (SessionStatus::Done, "exit 0".to_owned()),
                        Some(code) => (SessionStatus::Failed, format!("exit {code}")),
                        None => (SessionStatus::Failed, "terminated".to_owned()),
                    };
                }
                self.changed();
            }
            Event::Codex {
                session,
                generation,
                msg,
            } => {
                if let Some(agent) = self.agents.get_mut(&session)
                    && agent.generation == generation
                {
                    agent.handle(msg);
                    self.agent_changed(session);
                }
            }
            Event::CodexExited {
                session,
                generation,
            } => {
                if let Some(agent) = self.agents.get_mut(&session)
                    && agent.generation == generation
                {
                    agent.exited();
                    self.agent_changed(session);
                }
            }
        }
    }

    fn message(&mut self, client: u64, msg: ClientMsg) {
        match msg {
            ClientMsg::Hello { .. } => {}
            ClientMsg::Shutdown => {
                let _ = self.stop.send(());
            }
            ClientMsg::CreateGroup { name, path } => {
                self.store.create_group(&name, path);
                self.changed();
            }
            ClientMsg::RenameGroup { id, name } => {
                self.store.rename_group(id, &name);
                self.changed();
            }
            ClientMsg::SetGroupPath { id, path } => {
                self.store.set_group_path(id, path);
                self.changed();
            }
            ClientMsg::DeleteGroup { id } => {
                self.store.delete_group(id);
                self.changed();
            }
            ClientMsg::ToggleGroup { id } => {
                self.store.toggle_group(id);
                self.changed();
            }
            ClientMsg::CreateSession { group, title, kind } => {
                let id = self.store.create_session(group, &title, kind);
                self.send(client, ServerMsg::Created { id });
                self.changed();
            }
            ClientMsg::DeleteSession { id } => {
                self.processes.remove(&id);
                self.agents.remove(&id);
                let _ = std::fs::remove_file(self.agent_path(id));
                self.attached.remove(&id);
                self.store.delete_session(id);
                self.changed();
            }
            ClientMsg::MoveSession { id, group, index } => {
                self.store.move_session(id, group, index);
                self.changed();
            }
            ClientMsg::Attach { id } => {
                self.attached.entry(id).or_default().insert(client);
                let idle = self
                    .store
                    .session(id)
                    .is_some_and(|s| s.status == SessionStatus::Idle);
                if !self.processes.contains_key(&id) && idle {
                    self.spawn(id);
                }
                if let Some(msg) = self.attached_msg(id) {
                    self.send(client, msg);
                }
            }
            ClientMsg::Detach { id } => {
                if let Some(watchers) = self.attached.get_mut(&id) {
                    watchers.remove(&client);
                }
            }
            ClientMsg::Restart { id } => {
                self.processes.remove(&id);
                self.spawn(id);
                if let Some(msg) = self.attached_msg(id) {
                    self.send_watchers(id, msg);
                }
            }
            ClientMsg::Input { id, data } => {
                if let Some(process) = self.processes.get_mut(&id) {
                    process.write(&data);
                }
            }
            ClientMsg::Resize { id, cols, rows } => {
                if let Some(process) = self.processes.get_mut(&id) {
                    process.resize(cols, rows);
                }
            }
            ClientMsg::AgentOpen { id } => {
                self.attached.entry(id).or_default().insert(client);
                let agent = self.agent(id);
                let msg = ServerMsg::AgentTranscript {
                    id,
                    items: agent.items.clone(),
                    busy: agent.busy,
                };
                self.send(client, msg);
            }
            ClientMsg::AgentSend { id, text } => self.agent_send(id, text),
            ClientMsg::AgentDecide {
                id,
                approval,
                decision,
            } => {
                self.agent(id).decide(approval, decision);
                self.agent_changed(id);
            }
            ClientMsg::AgentStop { id } => {
                self.agent(id).stop();
                self.agent_changed(id);
            }
        }
    }

    fn spawn(&mut self, id: u64) {
        let Some(info) = self.store.session(id) else {
            return;
        };
        self.generation += 1;
        let spawned = Process::spawn(
            id,
            self.generation,
            info.kind,
            &info.title,
            self.store.session_path(id),
            self.events.clone(),
        );
        let Some(info) = self.store.session_mut(id) else {
            return;
        };
        match spawned {
            Ok(process) => {
                self.processes.insert(id, process);
                info.status = SessionStatus::Running;
            }
            Err(err) => {
                info.status = SessionStatus::Failed;
                info.meta = err.to_string();
            }
        }
        self.changed();
    }

    fn agent_path(&self, id: u64) -> PathBuf {
        self.state
            .with_file_name("agents")
            .join(format!("{id}.json"))
    }

    fn agent(&mut self, id: u64) -> &mut Agent {
        let path = self.agent_path(id);
        self.agents.entry(id).or_insert_with(|| Agent::load(&path))
    }

    fn agent_send(&mut self, id: u64, text: String) {
        let Some(info) = self.store.session_mut(id) else {
            return;
        };
        // a fresh agent takes its name from the first thing it's asked
        if info.title == "New agent" {
            let first = text.lines().next().unwrap_or_default().trim();
            let mut title: String = first.chars().take(48).collect();
            if first.chars().count() > 48 {
                title.push('…');
            }
            if !title.is_empty() {
                info.title = title;
            }
        }
        let folder = self.store.session_path(id).map(PathBuf::from);
        self.generation += 1;
        let (generation, events) = (self.generation, self.events.clone());
        let agent = self.agent(id);
        // an agent's sandbox is its folder, so falling back to the home folder would expose everything
        match folder {
            None => agent.error(
                "This group has no project folder yet. Choose one before starting an agent."
                    .to_owned(),
            ),
            Some(dir) if !dir.is_dir() => {
                agent.error(format!(
                    "The project folder {} doesn't exist any more.",
                    mux_core::display_path(&dir.to_string_lossy())
                ));
            }
            Some(dir) => agent.send(text, id, generation, dir, &events),
        }
        self.agent_changed(id);
        self.changed();
    }

    // pushes what changed to the clients watching, and keeps the sidebar status in step
    fn agent_changed(&mut self, id: u64) {
        let path = self.agent_path(id);
        let Some(agent) = self.agents.get_mut(&id) else {
            return;
        };
        let (items, busy) = agent.take_changes();
        let status = agent.status();
        // saved when a turn starts or ends rather than on every streamed word
        if (busy.is_some() || (!agent.busy && !items.is_empty()))
            && let Err(err) = agent.save(&path)
        {
            eprintln!("mux: could not save {}: {err}", path.display());
        }
        for (index, item) in items {
            self.send_watchers(id, ServerMsg::AgentItem { id, index, item });
        }
        if let Some(busy) = busy {
            self.send_watchers(id, ServerMsg::AgentBusy { id, busy });
        }
        if let Some(info) = self.store.session_mut(id)
            && info.status != status
        {
            info.status = status;
            info.meta = "codex".to_owned();
            self.changed();
        }
    }

    fn live(&mut self, session: u64, generation: u64) -> Option<&mut Process> {
        self.processes
            .get_mut(&session)
            .filter(|p| p.generation == generation)
    }

    fn attached_msg(&self, id: u64) -> Option<ServerMsg> {
        let process = self.processes.get(&id)?;
        Some(ServerMsg::Attached {
            id,
            cols: process.cols,
            rows: process.rows,
            replay: process.replay.clone(),
        })
    }

    fn changed(&mut self) {
        if let Err(err) = self.store.save(&self.state) {
            eprintln!("mux: could not save {}: {err}", self.state.display());
        }
        self.broadcast_state();
    }

    fn state_msg(&self) -> ServerMsg {
        ServerMsg::State {
            groups: self.store.groups.clone(),
        }
    }

    fn broadcast_state(&self) {
        let msg = self.state_msg();
        for tx in self.clients.values() {
            let _ = tx.send(msg.clone());
        }
    }

    fn send(&self, client: u64, msg: ServerMsg) {
        if let Some(tx) = self.clients.get(&client) {
            let _ = tx.send(msg);
        }
    }

    fn send_watchers(&self, session: u64, msg: ServerMsg) {
        let Some(watchers) = self.attached.get(&session) else {
            return;
        };
        for client in watchers {
            self.send(*client, msg.clone());
        }
    }
}
