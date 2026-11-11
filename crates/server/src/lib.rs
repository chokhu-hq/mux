mod agent;
mod agents;
mod files;
mod history;
mod listener;
mod mcp;
mod messages;
mod models;
mod process;
mod skills;
mod store;
#[cfg(windows)]
mod tray;
mod usage;
mod worktree;

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use mux_core::protocol::{
    self, AgentItem, ClientMsg, LimitReset, McpState, ModelOption, PROTOCOL, ResetResult,
    ServerMsg, UsageWindow,
};
use mux_core::{Provider, SessionStatus};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Notify;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use agent::Agent;
use process::{Change, Process};
use store::Store;

pub enum Event {
    #[cfg(windows)]
    Tray(tray::Action),
    Connected(u64, Client),
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
    // the project's files, listed for a session's `@` menu
    Files(u64, Vec<String>),
    // an agent's usage limit has reset, given with the reset it was waiting for
    Wake(u64, u64),
    // how spending one of an account's limit resets went
    ResetUsed(Provider, ResetResult),
    // more output from a command an agent left running in the background
    BackgroundOutput {
        session: u64,
        generation: u64,
        task: String,
        output: String,
    },
}

// blocks until ctrl+c or a client asks the server to shut down
pub fn run() -> io::Result<()> {
    mux_core::logging::init("server.log");
    run_at(
        protocol::socket_path(),
        protocol::data_dir().join("state.json"),
    )
}

pub fn run_at(socket: PathBuf, state: PathBuf) -> io::Result<()> {
    tokio::runtime::Runtime::new()?.block_on(serve(socket, state))
}

async fn serve(socket: PathBuf, state: PathBuf) -> io::Result<()> {
    let listener = listener::Listener::bind(&socket)?;
    log::info!(
        "mux server {} ({}) on {} {} listening on {}",
        mux_core::VERSION,
        mux_core::build_id(),
        std::env::consts::OS,
        std::env::consts::ARCH,
        listener.describe()
    );

    let (events, inbox) = mpsc::unbounded_channel();
    let (stop, mut stopped) = mpsc::unbounded_channel::<()>();
    // chats, logs and mcp keys all live in here, so other accounts on the machine are kept out
    if let Some(dir) = state.parent()
        && let Err(err) = mux_core::private_dir(dir)
    {
        log::warn!("couldn't make {} private: {err}", dir.display());
    }
    let mut store = Store::load(&state);
    let seen = store.sessions_mut().map(|s| (s.id, s.status)).collect();
    store.save(&state)?;
    let server = Server {
        mcp_status: Vec::new(),
        store,
        state,
        clients: HashMap::new(),
        processes: HashMap::new(),
        agents: HashMap::new(),
        seen,
        models: HashMap::new(),
        fetching_models: HashSet::new(),
        usage: HashMap::new(),
        resets: HashMap::new(),
        history: Some(Box::default()),
        attached: HashMap::new(),
        generation: 0,
        events: events.clone(),
        stop,
        leaving: false,
    };
    tokio::spawn(server.run(inbox));
    // a server on windows shows in the tray, since without a window it's otherwise invisible
    #[cfg(windows)]
    tray::start(events.clone());

    let mut next_client = 0;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                next_client += 1;
                listener.serve(accepted?, next_client, events.clone());
            }
            _ = tokio::signal::ctrl_c() => break,
            _ = stopped.recv() => break,
        }
    }
    listener.remove();
    #[cfg(windows)]
    tray::stop();
    Ok(())
}

// what an agent is told when it picks up a turn that a crash or forced quit cut off
const CUT_OFF: &str = "Mux restarted while you were working, so your last turn was cut off. Carry on where you left off. Anything you already finished, like edits, commands or commits, is done, so check before repeating it.";
const MAX_RESUMES: u32 = 3;
// and when a usage limit that stopped it has reset
const LIMIT_RESET: &str = "Your usage limit has reset, so carry on where you left off. Anything you already finished, like edits, commands or commits, is done, so check before repeating it.";
// seconds past a limit's reset before carrying on, since the clis round the time they report
const WAKE_SLACK: u64 = 5;

// how much may wait to be sent to one client, past which it's cut off rather than held for, like a window that
// froze while watching a long reply. it reconnects and is sent everything afresh once it reads again
const CLIENT_BACKLOG: usize = 32 * 1024 * 1024;

// a connected client's outbox, which counts what's queued so a client that stops reading can't grow the server
pub struct Client {
    // each frame with how much of it counts toward the backlog
    tx: UnboundedSender<(Vec<u8>, usize)>,
    queued: Arc<AtomicUsize>,
    cut: Arc<Notify>,
    over: AtomicBool,
}

impl Client {
    pub(crate) fn send(&self, msg: ServerMsg) -> Result<(), ()> {
        let Ok(frame) = protocol::encode(&msg) else {
            return Err(());
        };
        // the client hangs up on a frame this big, so sending it would only lose the connection
        if frame.len() > protocol::MAX_FRAME + 4 {
            log::error!("dropped a message of {} bytes", frame.len());
            return Err(());
        }
        if self.queued.load(Ordering::Relaxed) + frame.len() > CLIENT_BACKLOG {
            self.cut_off();
            return Err(());
        }
        self.queued.fetch_add(frame.len(), Ordering::Relaxed);
        let counted = frame.len();
        self.tx.send((frame, counted)).map_err(drop)
    }

    fn cut_off(&self) {
        if !self.over.swap(true, Ordering::Relaxed) {
            self.cut.notify_one();
        }
    }

    // a transcript the client asked for can outgrow the backlog by itself, and it has to arrive for the chat to open.
    // only up to half the backlog of it counts, so a client that keeps opening chats without reading is still cut off
    fn send_transcript(&self, pieces: &[ServerMsg]) {
        if self.queued.load(Ordering::Relaxed) >= CLIENT_BACKLOG {
            self.cut_off();
            return;
        }
        let mut room = CLIENT_BACKLOG / 2;
        for piece in pieces {
            if let Ok(frame) = protocol::encode(piece) {
                let counted = frame.len().min(room);
                room -= counted;
                self.queued.fetch_add(counted, Ordering::Relaxed);
                let _ = self.tx.send((frame, counted));
            }
        }
    }
}

// a long chat can pass the frame limit, so it goes out in pieces of about this size
const TRANSCRIPT_PIECE: usize = protocol::MAX_FRAME / 2;

// a note stands in for one item too big to send whole, so the rest of the chat can still open
fn fit_to_send(item: AgentItem) -> AgentItem {
    let size = protocol::encode(&item).map_or(0, |frame| frame.len());
    if size <= TRANSCRIPT_PIECE {
        return item;
    }
    AgentItem::Error {
        message: format!(
            "This step is too big to show ({} MB).",
            size / (1024 * 1024)
        ),
    }
}

fn transcript_pieces(id: u64, agent: &Agent) -> Vec<ServerMsg> {
    let mut pieces: Vec<Vec<AgentItem>> = vec![Vec::new()];
    let mut used = 0;
    for item in &agent.items {
        let item = fit_to_send(item.clone());
        let size = protocol::encode(&item).map_or(0, |frame| frame.len());
        if used + size > TRANSCRIPT_PIECE {
            pieces.push(Vec::new());
            used = 0;
        }
        used += size;
        pieces.last_mut().unwrap().push(item);
    }
    let mut pieces = pieces.into_iter();
    let mut msgs = vec![ServerMsg::AgentTranscript {
        id,
        items: pieces.next().unwrap_or_default(),
        busy: agent.busy,
        settings: agent.settings.clone(),
        context: agent.context,
    }];
    msgs.extend(pieces.map(|items| ServerMsg::AgentTranscriptMore { id, items }));
    msgs
}

pub(crate) async fn connection<S>(stream: S, id: u64, events: UnboundedSender<Event>)
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let (tx, mut outbox) = mpsc::unbounded_channel::<(Vec<u8>, usize)>();
    let queued = Arc::new(AtomicUsize::new(0));
    let cut = Arc::new(Notify::new());
    let client = Client {
        tx,
        queued: queued.clone(),
        cut: cut.clone(),
        over: AtomicBool::new(false),
    };
    if events.send(Event::Connected(id, client)).is_err() {
        return;
    }
    let send = tokio::spawn(async move {
        while let Some((frame, counted)) = outbox.recv().await {
            if writer.write_all(&frame).await.is_err() {
                break;
            }
            queued.fetch_sub(counted, Ordering::Relaxed);
        }
    });

    let read = async {
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
                    log::warn!("dropping client {id}, bad message: {err}");
                    break;
                }
            }
        }
    };
    tokio::select! {
        _ = read => {}
        _ = cut.notified() => {
            log::warn!("dropping client {id}, it stopped reading and fell too far behind");
        }
    }
    let _ = events.send(Event::Disconnected(id));
    send.abort();
}

struct Server {
    // how each of mux's mcp servers last started in each cli, for settings to show
    mcp_status: Vec<(String, Provider, McpState)>,
    store: Store,
    state: PathBuf,
    clients: HashMap<u64, Client>,
    processes: HashMap<u64, Process>,
    agents: HashMap<u64, Agent>,
    // last known status of each session, to notice when one changes
    seen: HashMap<u64, SessionStatus>,
    // what each agent cli can run, fetched once the first agent is opened
    models: HashMap<Provider, Vec<ModelOption>>,
    fetching_models: HashSet<Provider>,
    // the latest plan limits each agent cli reported
    usage: HashMap<Provider, Vec<UsageWindow>>,
    // the limit resets each agent's account has banked
    resets: HashMap<Provider, Vec<LimitReset>>,
    // token use read from the clis' transcripts, away on a blocking thread while it scans
    history: Option<Box<history::History>>,
    // which clients are watching each session's terminal
    attached: HashMap<u64, HashSet<u64>>,
    // bumped per spawn so output from a killed process can't leak into its replacement
    generation: u64,
    events: UnboundedSender<Event>,
    // set once the server is quitting on purpose
    leaving: bool,
    stop: UnboundedSender<()>,
}

impl Server {
    async fn run(mut self, mut inbox: UnboundedReceiver<Event>) {
        self.resume_cut_off();
        for (&id, &reset) in &self.store.wake {
            self.wake_at(id, reset);
        }
        while let Some(event) = inbox.recv().await {
            self.handle(event);
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            #[cfg(windows)]
            Event::Tray(tray::Action::Open) => {
                if self.clients.is_empty() {
                    // the server is the app's own binary, so starting it without --server opens a window
                    match std::env::current_exe().and_then(|exe| mux_core::command(exe).spawn()) {
                        Ok(_) => log::info!("opened a window from the tray"),
                        Err(e) => log::warn!("couldn't open a window from the tray: {e}"),
                    }
                } else {
                    for tx in self.clients.values() {
                        let _ = tx.send(ServerMsg::Focus);
                    }
                }
            }
            #[cfg(windows)]
            Event::Tray(tray::Action::Quit) => {
                log::info!("quitting from the tray");
                self.quitting();
                for tx in self.clients.values() {
                    let _ = tx.send(ServerMsg::Quit);
                }
                // a moment for the windows to hear it before the connections go
                let stop = self.stop.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let _ = stop.send(());
                });
            }
            Event::Connected(client, tx) => {
                log::info!("client {client} connected");
                let _ = tx.send(ServerMsg::Welcome { protocol: PROTOCOL });
                let _ = tx.send(ServerMsg::Build {
                    id: mux_core::build_id().to_owned(),
                });
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
                for (provider, resets) in &self.resets {
                    let _ = tx.send(ServerMsg::AgentResets {
                        provider: *provider,
                        resets: resets.clone(),
                    });
                }
                let _ = tx.send(ServerMsg::McpServers {
                    servers: self.store.mcp.clone(),
                });
                let _ = tx.send(ServerMsg::McpStatus {
                    statuses: self.mcp_status.clone(),
                });
                self.clients.insert(client, tx);
            }
            Event::Disconnected(client) => {
                log::info!("client {client} disconnected");
                self.clients.remove(&client);
                for watchers in self.attached.values_mut() {
                    watchers.remove(&client);
                }
            }
            Event::Client(client, msg) => self.message(client, msg),
            Event::Wake(id, reset) => self.wake(id, reset),
            Event::ResetUsed(provider, result) => self.reset_used(provider, result),
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
            Event::Files(id, files) => {
                self.send_watchers(id, ServerMsg::AgentFiles { id, files });
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
                        log::info!(
                            "listed {} {} models",
                            catalog.models.len(),
                            provider.label()
                        );
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
                        if let Some(resets) = catalog.resets {
                            self.set_resets(provider, resets);
                        }
                    }
                    Err(err) => log::error!("could not list {} models: {err}", provider.label()),
                }
            }
            Event::BackgroundOutput {
                session,
                generation,
                task,
                output,
            } => {
                if let Some(agent) = self.agents.get_mut(&session)
                    && agent.generation == generation
                {
                    agent.background_output(&task, output);
                    self.agent_changed(session);
                }
            }
            Event::AgentExited {
                session,
                generation,
            } => {
                if let Some(agent) = self.agents.get_mut(&session)
                    && agent.generation == generation
                {
                    log::info!(
                        "session {session}: {} exited{}",
                        agent.settings.provider.label(),
                        if agent.busy {
                            " in the middle of a turn"
                        } else {
                            ""
                        }
                    );
                    agent.exited();
                    self.agent_changed(session);
                }
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

    // quitting on purpose stops what's running, so the next start doesn't pick it back up as if it crashed
    fn quitting(&mut self) {
        self.leaving = true;
        for info in self.store.sessions_mut() {
            if info.status == SessionStatus::Running {
                info.status = SessionStatus::Idle;
            }
        }
        if let Err(err) = self.store.save(&self.state) {
            log::error!("could not save {}: {err}", self.state.display());
        }
    }

    fn broadcast_mcp_status(&self) {
        for tx in self.clients.values() {
            let _ = tx.send(ServerMsg::McpStatus {
                statuses: self.mcp_status.clone(),
            });
        }
    }

    fn set_resets(&mut self, provider: Provider, resets: Vec<LimitReset>) {
        if self.resets.get(&provider) == Some(&resets) {
            return;
        }
        for tx in self.clients.values() {
            let _ = tx.send(ServerMsg::AgentResets {
                provider,
                resets: resets.clone(),
            });
        }
        self.resets.insert(provider, resets);
    }

    // a used reset lifts the limits now, so the agents waiting on them carry on rather than wait for the clock
    fn reset_used(&mut self, provider: Provider, result: ResetResult) {
        log::info!("used a {} limit reset: {result:?}", provider.label());
        for tx in self.clients.values() {
            let _ = tx.send(ServerMsg::ResetResult {
                provider,
                result: result.clone(),
            });
        }
        if result == ResetResult::Reset {
            let waiting: Vec<(u64, u64)> = self
                .store
                .wake
                .iter()
                .filter(|(id, _)| {
                    self.store
                        .session(**id)
                        .is_some_and(|s| s.provider == provider)
                })
                .map(|(&id, &reset)| (id, reset))
                .collect();
            for (id, reset) in waiting {
                self.wake(id, reset);
            }
        }
        // the limits and the resets left both changed
        self.fetch_models(provider);
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
            log::error!("could not save {}: {err}", self.state.display());
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
        // submodules point there relative to their own folder
        Path::new(folder).join(pointer.strip_prefix("gitdir:")?.trim())
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
    fn a_client_that_keeps_opening_big_chats_without_reading_is_cut_off() {
        let (tx, _outbox) = mpsc::unbounded_channel();
        let client = Client {
            tx,
            queued: Arc::new(AtomicUsize::new(0)),
            cut: Arc::new(Notify::new()),
            over: AtomicBool::new(false),
        };
        let big = |n: u64| ServerMsg::AgentTranscriptMore {
            id: n,
            items: vec![AgentItem::Assistant {
                text: "x".repeat(TRANSCRIPT_PIECE - 1024),
            }],
        };
        let chat: Vec<ServerMsg> = (0..6).map(big).collect();
        client.send_transcript(&chat);
        assert!(
            !client.over.load(Ordering::Relaxed),
            "one long chat still opens"
        );
        assert!(
            client.send(ServerMsg::Focus).is_ok(),
            "and what follows it still fits"
        );
        client.send_transcript(&chat);
        client.send_transcript(&chat);
        assert!(client.over.load(Ordering::Relaxed));
    }

    #[test]
    fn shortens_shell_titles_to_their_folder() {
        // the home folder and separators of whichever system runs the test
        let project = mux_core::home_dir().join("code").join("mux");
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(
            short_title(&format!("me@box:{}", project.display())),
            format!("~{sep}code{sep}mux")
        );
        assert_eq!(short_title("me@box:~"), "~");
        assert_eq!(short_title("vim notes.md"), "vim notes.md");
        assert_eq!(short_title("ssh: connecting"), "ssh: connecting");
    }

    #[test]
    fn reads_the_branch_of_this_repo() {
        // this crate sits at crates/server inside the repo
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(git_branch(&repo).is_some_and(|b| !b.is_empty()));
        assert_eq!(git_branch("/nonexistent"), None);
    }
}
