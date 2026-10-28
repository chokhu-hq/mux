use std::io;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui;
use mux_core::protocol::{ClientMsg, PROTOCOL, ServerMsg};

use crate::connection::Connection;
use crate::sessions::Sessions;

const RETRY_SECONDS: f64 = 2.0;

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
        let down = |error: String| Link::Down {
            error,
            retry_at: now + RETRY_SECONDS,
        };
        let placeholder = Link::Down {
            error: String::new(),
            retry_at: 0.0,
        };
        *self = match std::mem::replace(self, placeholder) {
            Link::Connecting { result, since } => match result.try_recv() {
                Ok(Ok(connection)) => {
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
                        msg => sessions.apply(msg),
                    }
                }
                if outdated.is_none() && connection.is_unreadable() {
                    outdated = Some(None);
                }
                if let Some(protocol) = outdated {
                    sessions.outbox.clear();
                    Link::Outdated {
                        connection,
                        protocol,
                    }
                } else {
                    for msg in sessions.outbox.drain(..) {
                        connection.send(&msg);
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
            Link::Outdated { mut connection, .. } => {
                connection.send(&ClientMsg::Shutdown);
                Link::Stopping(connection)
            }
            other => other,
        };
    }
}
