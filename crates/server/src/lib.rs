mod agent;
mod history;
mod models;
mod process;
mod skills;
mod store;
mod usage;
mod worktree;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};

use mux_core::protocol::{self, ClientMsg, ModelOption, PROTOCOL, ServerMsg, UsageWindow};
use mux_core::{Provider, SessionStatus};
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
    Agent {
        session: u64,
        generation: u64,
        msg: serde_json::Value,
    },
    AgentExited {
        session: u64,
        generation: u64,
    },
    Models(Provider, Result<models::Catalog, String>),
    History(Box<history::History>),
    // the skills found for a session's agent, tagged with which agent they're for
    Skills(u64, Provider, Vec<mux_core::protocol::Skill>),
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
    let mut store = Store::load(&state);
    let seen = store.sessions_mut().map(|s| (s.id, s.status)).collect();
    store.save(&state)?;
    let server = Server {
        store,
        state,
        clients: HashMap::new(),
        processes: HashMap::new(),
        agents: HashMap::new(),
        seen,
        models: HashMap::new(),
        fetching_models: HashSet::new(),
        usage: HashMap::new(),
        history: Some(Box::default()),
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
    // last known status of each session, to notice when one changes
    seen: HashMap<u64, SessionStatus>,
    // what each agent cli can run, fetched once the first agent is opened
    models: HashMap<Provider, Vec<ModelOption>>,
    fetching_models: HashSet<Provider>,
    // the latest plan limits each agent cli reported
    usage: HashMap<Provider, Vec<UsageWindow>>,
    // token use read from the clis' transcripts, away on a blocking thread while it scans
    history: Option<Box<history::History>>,
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
                for (provider, models) in &self.models {
                    let _ = tx.send(ServerMsg::AgentModels {
                        provider: *provider,
                        models: models.clone(),
                    });
                }
                for (provider, windows) in &self.usage {
                    let _ = tx.send(ServerMsg::AgentUsage {
                        provider: *provider,
                        windows: windows.clone(),
                    });
                }
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
                        && let Some(title) = title.map(|t| short_title(&t))
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
            Event::Agent {
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
            Event::Skills(id, provider, skills) => {
                // a switch of agent since asking makes these the wrong agent's skills
                let agent = self.agent(id);
                if agent.settings.provider == provider {
                    agent.set_skills(skills);
                    self.agent_changed(id);
                }
            }
            Event::History(history) => {
                let (today, days) = history.summary();
                self.history = Some(history);
                for tx in self.clients.values() {
                    let _ = tx.send(ServerMsg::TokenHistory {
                        today,
                        days: days.clone(),
                    });
                }
            }
            Event::Models(provider, result) => {
                self.fetching_models.remove(&provider);
                match result {
                    Ok(catalog) => {
                        for tx in self.clients.values() {
                            let _ = tx.send(ServerMsg::AgentModels {
                                provider,
                                models: catalog.models.clone(),
                            });
                        }
                        self.models.insert(provider, catalog.models);
                        if let Some(windows) = catalog.usage {
                            self.set_usage(provider, windows);
                        }
                    }
                    Err(err) => eprintln!("mux: could not list {} models: {err}", provider.label()),
                }
            }
            Event::AgentExited {
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
            ClientMsg::MoveGroup { id, index } => {
                self.store.move_group(id, index);
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
                if let Some(path) = self.store.session(id).and_then(|s| s.worktree.clone()) {
                    worktree::remove(path);
                }
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
                    settings: agent.settings.clone(),
                    context: agent.context,
                };
                let (skills, provider) = (agent.skills(), agent.settings.provider);
                self.send(client, msg);
                self.send(client, ServerMsg::AgentSkills { id, skills });
                self.fetch_skills(id, provider);
                for provider in Provider::ALL {
                    if !self.models.contains_key(&provider) {
                        self.fetch_models(provider);
                    }
                }
            }
            ClientMsg::RefreshUsage => {
                for provider in Provider::ALL {
                    self.fetch_models(provider);
                }
                // a scan already running answers this request too
                if let Some(mut history) = self.history.take() {
                    let events = self.events.clone();
                    tokio::task::spawn_blocking(move || {
                        history.scan();
                        let _ = events.send(Event::History(history));
                    });
                }
            }
            ClientMsg::AgentConfigure { id, mut settings } => {
                let path = self.agent_path(id);
                let agent = self.agent(id);
                let before = agent.settings.provider;
                // a conversation stays with the agent and the folder it started in
                if !agent.items.is_empty() {
                    settings.provider = agent.settings.provider;
                    settings.worktree = agent.settings.worktree;
                }
                agent.settings = settings.clone();
                if let Err(err) = agent.save(&path) {
                    eprintln!("mux: could not save {}: {err}", path.display());
                }
                let summary = agent.summary();
                // the other agent has its own skills, so the menu needs them instead
                if before != settings.provider {
                    agent.set_skills(Vec::new());
                }
                if let Some(info) = self.store.session_mut(id)
                    && info.summary.as_ref() != Some(&summary)
                {
                    info.summary = Some(summary);
                    self.changed();
                }
                if let Some(info) = self.store.session_mut(id)
                    && info.provider != settings.provider
                {
                    info.provider = settings.provider;
                    self.changed();
                }
                let switched = before != settings.provider;
                self.send_watchers(id, ServerMsg::AgentSettings { id, settings });
                if switched {
                    self.agent_changed(id);
                    let provider = self.agent(id).settings.provider;
                    self.fetch_skills(id, provider);
                }
            }
            ClientMsg::AgentSend {
                id,
                text,
                attachments,
            } => self.agent_send(id, text, attachments),
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
            ClientMsg::AgentCompact { id } => self.agent_compact(id),
            ClientMsg::AgentAnswer {
                id,
                question,
                answers,
            } => {
                self.agent(id).answer(question, answers);
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

    fn agent_send(&mut self, id: u64, text: String, attachments: Vec<String>) {
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
        // the first message of a worktree agent makes its checkout, named after what it was asked
        let wants_worktree = info.worktree.is_none()
            && self
                .agents
                .get(&id)
                .is_some_and(|a| a.settings.worktree && a.items.is_empty());
        if wants_worktree {
            let title = self
                .store
                .session(id)
                .map(|s| s.title.clone())
                .unwrap_or_default();
            match self
                .store
                .project_path(id)
                .map(|repo| worktree::add(repo, &title, id))
            {
                Some(Ok(path)) => {
                    if let Some(info) = self.store.session_mut(id) {
                        info.worktree = Some(path);
                    }
                }
                Some(Err(reason)) => {
                    self.agent(id).error(reason);
                    self.agent_changed(id);
                    self.changed();
                    return;
                }
                None => {}
            }
        }
        // plan mode needs codex's model named, so the default stands in when none was picked
        let default_model = self
            .models
            .get(&Provider::Codex)
            .and_then(|list| list.iter().find(|m| m.is_default))
            .map(|m| m.id.clone());
        self.agent(id).default_model = default_model;
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
            Some(dir) => agent.send(text, attachments, id, generation, dir, &events),
        }
        self.agent_changed(id);
        self.changed();
    }

    // skills are read fresh each time a chat opens, so a new or edited one shows up without restarting
    fn fetch_skills(&self, id: u64, provider: Provider) {
        let Some(folder) = self.store.session_path(id).map(PathBuf::from) else {
            return;
        };
        let events = self.events.clone();
        tokio::spawn(async move {
            let skills = match provider {
                Provider::Claude => tokio::task::spawn_blocking(move || skills::claude(&folder))
                    .await
                    .unwrap_or_default(),
                Provider::Codex => skills::codex(&folder).await.unwrap_or_default(),
            };
            let _ = events.send(Event::Skills(id, provider, skills));
        });
    }

    // the model list comes with the plan limits, so fetching it again is how usage is refreshed
    fn fetch_models(&mut self, provider: Provider) {
        if !self.fetching_models.insert(provider) {
            return;
        }
        let events = self.events.clone();
        tokio::spawn(async move {
            let result = models::fetch(provider).await.map_err(|e| e.to_string());
            let _ = events.send(Event::Models(provider, result));
        });
    }

    fn agent_compact(&mut self, id: u64) {
        let Some(dir) = self
            .store
            .session_path(id)
            .map(PathBuf::from)
            .filter(|dir| dir.is_dir())
        else {
            return;
        };
        self.generation += 1;
        let (generation, events) = (self.generation, self.events.clone());
        self.agent(id).compact(id, generation, dir, &events);
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
        let provider = agent.settings.provider;
        let label = provider.label();
        let usage = agent.usage.take();
        let context = agent.take_context();
        let skills = agent.take_skills();
        // saved when a turn starts or ends rather than on every streamed word, and the sidebar's summary with it
        let settled = busy.is_some() || (!agent.busy && !items.is_empty());
        if settled && let Err(err) = agent.save(&path) {
            eprintln!("mux: could not save {}: {err}", path.display());
        }
        let summary = settled.then(|| agent.summary());
        for (index, item) in items {
            self.send_watchers(id, ServerMsg::AgentItem { id, index, item });
        }
        if let Some(busy) = busy {
            self.send_watchers(id, ServerMsg::AgentBusy { id, busy });
        }
        if let Some(windows) = usage {
            self.set_usage(provider, windows);
        }
        if let Some(context) = context {
            self.send_watchers(id, ServerMsg::AgentContext { id, context });
        }
        if let Some(skills) = skills {
            self.send_watchers(id, ServerMsg::AgentSkills { id, skills });
        }
        if let Some(info) = self.store.session_mut(id)
            && info.status != status
        {
            info.status = status;
            info.meta = label.to_owned();
            self.changed();
        }
        if let (Some(summary), Some(info)) = (summary, self.store.session_mut(id))
            && info.summary.as_ref() != Some(&summary)
        {
            info.summary = Some(summary);
            self.changed();
        }
    }

    fn set_usage(&mut self, provider: Provider, windows: Vec<UsageWindow>) {
        if self.usage.get(&provider) == Some(&windows) {
            return;
        }
        for tx in self.clients.values() {
            let _ = tx.send(ServerMsg::AgentUsage {
                provider,
                windows: windows.clone(),
            });
        }
        self.usage.insert(provider, windows);
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
        // stamp sessions whose status moved, so the sidebar can say how long ago it happened
        for info in self.store.sessions_mut() {
            if self.seen.insert(info.id, info.status) != Some(info.status) {
                info.updated = store::now();
            }
        }
        if let Err(err) = self.store.save(&self.state) {
            eprintln!("mux: could not save {}: {err}", self.state.display());
        }
        self.broadcast_state();
    }

    fn state_msg(&self) -> ServerMsg {
        let mut groups = self.store.groups.clone();
        for group in &mut groups {
            group.branch = group.path.as_deref().and_then(git_branch);
            for session in &mut group.sessions {
                session.branch = session.worktree.as_deref().and_then(git_branch);
            }
        }
        ServerMsg::State { groups }
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

// shells title themselves `user@host:path`, and only the path is worth showing
fn short_title(title: &str) -> String {
    let path = match title.split_once(':') {
        Some((who, path)) if who.contains('@') && !who.contains(' ') => path.trim(),
        _ => title,
    };
    mux_core::display_path(path)
}

// reads the branch straight from .git so showing state never has to run git
fn git_branch(folder: &str) -> Option<String> {
    let dot_git = Path::new(folder).join(".git");
    let git_dir = if dot_git.is_file() {
        // worktrees and submodules point at their real git directory
        let pointer = std::fs::read_to_string(&dot_git).ok()?;
        PathBuf::from(pointer.strip_prefix("gitdir:")?.trim())
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    match head.trim().strip_prefix("ref: refs/heads/") {
        Some(branch) => Some(branch.to_owned()),
        None => Some(head.trim().chars().take(7).collect()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortens_shell_titles_to_their_folder() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(
            short_title(&format!("me@box:{home}/code/mux")),
            "~/code/mux"
        );
        assert_eq!(short_title("me@box:~"), "~");
        assert_eq!(short_title("vim notes.md"), "vim notes.md");
        assert_eq!(short_title("ssh: connecting"), "ssh: connecting");
    }

    #[test]
    fn reads_the_branch_of_this_repo() {
        let repo = env!("CARGO_MANIFEST_DIR")
            .rsplit_once("/crates/")
            .unwrap()
            .0;
        assert!(git_branch(repo).is_some_and(|b| !b.is_empty()));
        assert_eq!(git_branch("/nonexistent"), None);
    }
}
