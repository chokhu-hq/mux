use std::fs::OpenOptions;
use std::io;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;
use mux_core::protocol::{self, ClientMsg, PROTOCOL, ServerMsg};
use mux_core::transport::{self, Stream};

pub struct Connection {
    stream: Stream,
    inbox: Receiver<ServerMsg>,
    closed: Arc<AtomicBool>,
    unreadable: Arc<AtomicBool>,
}

impl Connection {
    // connects to the running server, starting one in the background when there is none
    pub fn open(ctx: &egui::Context) -> io::Result<Self> {
        let path = protocol::socket_path();
        let stream = match transport::connect(&path) {
            Ok(stream) => stream,
            Err(_) => {
                start_server()?;
                let deadline = Instant::now() + Duration::from_secs(3);
                loop {
                    match transport::connect(&path) {
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
        let unreadable = Arc::new(AtomicBool::new(false));
        let (flag, odd, ctx) = (closed.clone(), unreadable.clone(), ctx.clone());
        thread::spawn(move || {
            loop {
                match protocol::read::<ServerMsg>(&mut reader) {
                    Ok(msg) => {
                        if tx.send(msg).is_err() {
                            break;
                        }
                    }
                    // a whole frame arrived but isn't a message this build knows, so the server is from another one
                    Err(err) if err.kind() == io::ErrorKind::Other => {
                        odd.store(true, Ordering::Relaxed);
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
            unreadable,
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

    // the server sent something this build can't read, which only happens across versions
    pub fn is_unreadable(&self) -> bool {
        self.unreadable.load(Ordering::Relaxed)
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
    log::info!("starting a server");
    // the server writes its own log, and this only catches what it prints if it dies before it can
    let log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_dir.join("server-output.log"))?;

    let mut cmd = mux_core::command(std::env::current_exe()?);
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
