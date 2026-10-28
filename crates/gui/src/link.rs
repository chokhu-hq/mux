use std::io;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui;
use mux_core::protocol::{ClientMsg, PROTOCOL, ServerMsg};

use crate::connection::Connection;
use crate::sessions::Sessions;

const RETRY_SECONDS: f64 = 2.0;

// each run of the app restarts a server from another build by itself only once,
// so two different builds left open don't take turns restarting it forever
static RESTARTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub enum Link {
    Connecting {
        result: Receiver<io::Result<Connection>>,
        since: f64,
    },
    Up(Connection),
    // a server from another build is running, and its messages can't be read
    Outdated {
        connection: Connection,
        // what the server said it speaks, or none when its messages just didn't decode
        protocol: Option<u32>,
    },
    // asked an outdated server to quit, waiting for it to go before starting a fresh one
    Stopping(Connection),
    Down {
        error: String,
        retry_at: f64,
    },
}

impl Link {
    // connecting can mean starting a server and waiting on it, so it stays off the ui thread
    pub fn start(ctx: &egui::Context) -> Self {
        let (tx, result) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Connection::open(&ctx));
            ctx.request_repaint();
        });
        Link::Connecting { result, since: 0.0 }
    }

    pub fn tick(&mut self, ctx: &egui::Context, sessions: &mut Sessions) {
        let now = ctx.input(|i| i.time);
        let down = |error: String| {
            failed(&error);
            Link::Down {
                error,
                retry_at: now + RETRY_SECONDS,
            }
        };
        let placeholder = Link::Down {
            error: String::new(),
            retry_at: 0.0,
        };
        *self = match std::mem::replace(self, placeholder) {
            Link::Connecting { result, since } => match result.try_recv() {
                Ok(Ok(connection)) => {
                    log::info!("connected to the server");
                    failed("");
                    // a new connection replays every terminal, so drop the stale local copies
                    sessions.reset_terminals();
                    Link::Up(connection)
                }
                Ok(Err(err)) => down(err.to_string()),
                Err(TryRecvError::Empty) => Link::Connecting {
                    result,
                    since: if since == 0.0 { now } else { since },
                },
                Err(TryRecvError::Disconnected) => {
                    down("Connecting stopped unexpectedly".to_owned())
                }
            },
            Link::Up(mut connection) => {
                let mut outdated = None;
                for msg in connection.messages() {
                    match msg {
                        ServerMsg::Welcome { protocol } if protocol != PROTOCOL => {
                            outdated = Some(Some(protocol));
                            break;
                        }
                        ServerMsg::Build { id } => connection.server_build = Some(id),
                        msg => {
                            // a server that sends its state without saying its build is older than build ids
                            if matches!(msg, ServerMsg::State { .. })
                                && connection.server_build.is_none()
                            {
                                connection.server_build = Some("unknown".to_owned());
                            }
                            sessions.apply(msg)
                        }
                    }
                }
                if outdated.is_none() && connection.is_unreadable() {
                    outdated = Some(None);
                }
                if let Some(protocol) = outdated {
                    log::warn!(
                        "the server speaks protocol {}, and this app speaks {PROTOCOL}",
                        protocol.map_or("unknown".to_owned(), |p| p.to_string())
                    );
                    sessions.outbox.clear();
                    Link::Outdated {
                        connection,
                        protocol,
                    }
                } else if connection.is_stale()
                    && local()
                    && !sessions.is_busy()
                    && !RESTARTED.swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    // nothing is running in it, so it can be swapped for this build's without anyone losing work
                    log::info!(
                        "restarting the server from build {} into {}",
                        connection.server_build.as_deref().unwrap_or_default(),
                        mux_core::build_id()
                    );
                    sessions.outbox.clear();
                    connection.send(&ClientMsg::Shutdown);
                    Link::Stopping(connection)
                } else {
                    // nothing is sent until the server's build is known, since one from another build may not read it
                    if connection.server_build.is_some() {
                        for msg in sessions.outbox.drain(..) {
                            connection.send(&msg);
                        }
                    }
                    if connection.is_closed() {
                        down("The connection to the Mux server was lost".to_owned())
                    } else {
                        Link::Up(connection)
                    }
                }
            }
            Link::Outdated {
                connection,
                protocol,
            } => {
                connection.messages();
                sessions.outbox.clear();
                Link::Outdated {
                    connection,
                    protocol,
                }
            }
            Link::Stopping(connection) => {
                connection.messages();
                sessions.outbox.clear();
                if connection.is_closed() {
                    Link::start(ctx)
                } else {
                    ctx.request_repaint_after(Duration::from_millis(100));
                    Link::Stopping(connection)
                }
            }
            Link::Down { error, retry_at } => {
                // nothing can be delivered while offline, and replaying stale input later would be worse
                sessions.outbox.clear();
                if now >= retry_at {
                    Link::start(ctx)
                } else {
                    ctx.request_repaint_after(Duration::from_secs_f64(retry_at - now));
                    Link::Down { error, retry_at }
                }
            }
        };
    }

    pub fn is_up(&self) -> bool {
        matches!(self, Link::Up(_))
    }

    // a server from another build that's left running because something is running in it
    pub fn is_stale(&self) -> bool {
        matches!(self, Link::Up(connection) if connection.is_stale() && local())
    }

    pub fn reconnect_now(&mut self, ctx: &egui::Context) {
        if matches!(self, Link::Down { .. }) {
            *self = Link::start(ctx);
        }
    }

    // shutdown sits at the same spot in every protocol version, so an older server still understands it
    pub fn restart_server(&mut self) {
        let placeholder = Link::Down {
            error: String::new(),
            retry_at: 0.0,
        };
        *self = match std::mem::replace(self, placeholder) {
            Link::Outdated { mut connection, .. } | Link::Up(mut connection) => {
                connection.send(&ClientMsg::Shutdown);
                Link::Stopping(connection)
            }
            other => other,
        };
    }
}

// a server in wsl is installed from a release rather than run from this app's binary, so restarting it can't change its build
fn local() -> bool {
    #[cfg(windows)]
    return mux_core::paths::wsl().is_none();
    #[cfg(not(windows))]
    true
}

// retrying while the server is down fails the same way every few seconds, so only a new reason is logged
fn failed(error: &str) {
    static LAST: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
    let Ok(mut last) = LAST.lock() else {
        return;
    };
    if *last != error {
        if !error.is_empty() {
            log::warn!("server connection: {error}");
        }
        *last = error.to_owned();
    }
}
