use std::fs::OpenOptions;
use std::io;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use mux_core::protocol::{self, ClientMsg, PROTOCOL, ServerMsg};

pub struct Connection {
    stream: UnixStream,
    inbox: Receiver<ServerMsg>,
    closed: Arc<AtomicBool>,
}

impl Connection {
    // connects to the running server, starting one in the background when there is none
    pub fn open(ctx: &egui::Context) -> io::Result<Self> {
        let path = protocol::socket_path();
        let stream = match UnixStream::connect(&path) {
            Ok(stream) => stream,
            Err(_) => {
                start_server()?;
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    match UnixStream::connect(&path) {
                        Ok(stream) => break stream,
                        Err(_) if Instant::now() < deadline => {
                            thread::sleep(Duration::from_millis(25))
                        }
                        Err(_) => {
                            let log = protocol::data_dir().join("server.log");
                            return Err(io::Error::other(format!(
                                "Started a server but nothing answered at {}. Its log is at {}",
                                mux_core::display_path(&path.to_string_lossy()),
                                mux_core::display_path(&log.to_string_lossy()),
                            )));
                        }
                    }
                }
            }
        };

        let mut reader = stream.try_clone()?;
        let (tx, inbox) = mpsc::channel();
        let closed = Arc::new(AtomicBool::new(false));
        let (flag, ctx) = (closed.clone(), ctx.clone());
        thread::spawn(move || {
            loop {
                match protocol::read::<ServerMsg>(&mut reader) {
                    Ok(msg) => {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        flag.store(true, Ordering::Relaxed);
                        break;
                    }
                }
                ctx.request_repaint();
            }
            ctx.request_repaint();
        });

        let mut connection = Self {
            stream,
            inbox,
            closed,
        };
        connection.send(&ClientMsg::Hello { protocol: PROTOCOL });
        Ok(connection)
    }

    pub fn send(&mut self, msg: &ClientMsg) {
        if protocol::write(&mut self.stream, msg).is_err() {
            self.closed.store(true, Ordering::Relaxed);
        }
    }

    pub fn messages(&self) -> Vec<ServerMsg> {
        self.inbox.try_iter().collect()
    }

    // callers drain messages first, so anything the server sent before closing is already handled
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }
}

// the gui binary doubles as the server, so a release ships as one executable
fn start_server() -> io::Result<()> {
    let log_dir = protocol::data_dir();
    std::fs::create_dir_all(&log_dir)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_dir.join("server.log"))?;

    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.arg("--server")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // its own process group, so ctrl+c in the launching terminal leaves the server running
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    // reap it if it exits while the app is still open
    thread::spawn(move || child.wait());
    Ok(())
}
