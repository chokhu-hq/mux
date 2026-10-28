use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use alacritty_terminal::event::{Event as TermEvent, EventListener};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use mux_core::SessionKind;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use tokio::sync::mpsc::UnboundedSender;

use crate::Event;

const REPLAY_LIMIT: usize = 1024 * 1024;

#[derive(Clone)]
struct Listener(mpsc::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

pub enum Change {
    Title(Option<String>),
}

pub struct Process {
    pub generation: u64,
    pub cols: u16,
    pub rows: u16,
    pub exited: bool,
    // raw output kept so a client attaching later can rebuild the screen
    pub replay: Vec<u8>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    term: Term<Listener>,
    parser: Processor,
    term_events: mpsc::Receiver<TermEvent>,
}

impl Process {
    pub fn spawn(
        session: u64,
        generation: u64,
        kind: SessionKind,
        title: &str,
        folder: Option<&str>,
        events: UnboundedSender<Event>,
    ) -> io::Result<Self> {
        let cwd = match folder {
            Some(folder) if !Path::new(folder).is_dir() => {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("folder not found: {}", mux_core::display_path(folder)),
                ));
            }
            Some(folder) => Some(PathBuf::from(folder)),
            None => home_dir(),
        };
        let (cols, rows) = (80, 24);
        let pair = native_pty_system()
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| io::Error::other(e.to_string()))?;

        let mut cmd = match kind {
            SessionKind::Shell => login_shell(),
            SessionKind::Task => shell_command(title),
            SessionKind::Agent => shell_command(&agent_command()),
        };
        if let Some(cwd) = cwd {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| io::Error::other(e.to_string()))?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| io::Error::other(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| io::Error::other(e.to_string()))?;

        thread::spawn(move || {
            let mut buf = vec![0; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let data = buf[..n].to_vec();
                        let output = Event::Output {
                            session,
                            generation,
                            data,
                        };
                        if events.send(output).is_err() {
                            return;
                        }
                    }
                }
            }
            // a signal death has no exit code, which callers report as terminated
            let code = child
                .wait()
                .ok()
                .filter(|s| s.signal().is_none())
                .map(|s| s.exit_code() as i32);
            let _ = events.send(Event::Exited {
                session,
                generation,
                code,
            });
        });

        let (tx, term_events) = mpsc::channel();
        let term = Term::new(
            Config::default(),
            &TermSize::new(cols as usize, rows as usize),
            Listener(tx),
        );
        Ok(Self {
            generation,
            cols,
            rows,
            exited: false,
            replay: Vec::new(),
            master: pair.master,
            writer,
            killer,
            term,
            parser: Processor::new(),
            term_events,
        })
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Change> {
        self.parser.advance(&mut self.term, data);
        self.replay.extend_from_slice(data);
        if self.replay.len() > REPLAY_LIMIT {
            // trim to three quarters so the copy happens rarely rather than on every chunk
            let excess = self.replay.len() - REPLAY_LIMIT * 3 / 4;
            self.replay.drain(..excess);
        }

        let mut changes = Vec::new();
        while let Ok(event) = self.term_events.try_recv() {
            match event {
                // answers to queries like cursor position, which the program is waiting on
                TermEvent::PtyWrite(text) => self.write(text.as_bytes()),
                TermEvent::Title(title) => changes.push(Change::Title(Some(title))),
                TermEvent::ResetTitle => changes.push(Change::Title(None)),
                _ => {}
            }
        }
        changes
    }

    pub fn write(&mut self, data: &[u8]) {
        if self.exited {
            return;
        }
        let _ = self
            .writer
            .write_all(data)
            .and_then(|_| self.writer.flush());
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        if (cols, rows) == (self.cols, self.rows) || cols == 0 || rows == 0 {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term
            .resize(TermSize::new(cols as usize, rows as usize));
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        if !self.exited {
            let _ = self.killer.kill();
        }
    }
}

// the account's own shell as a login shell, since the server may have been started from a different one
#[cfg(unix)]
fn login_shell() -> CommandBuilder {
    let shell = mux_core::login_shell();
    let mut cmd = CommandBuilder::new(&shell);
    cmd.arg("-l");
    cmd.env("SHELL", shell);
    cmd
}

#[cfg(windows)]
fn login_shell() -> CommandBuilder {
    CommandBuilder::new_default_prog()
}

// runs through the login shell so the user's PATH and aliases apply
#[cfg(unix)]
fn shell_command(command: &str) -> CommandBuilder {
    let shell = mux_core::login_shell();
    let mut cmd = CommandBuilder::new(&shell);
    cmd.args(["-l", "-c", command]);
    cmd.env("SHELL", shell);
    cmd
}

#[cfg(windows)]
fn shell_command(command: &str) -> CommandBuilder {
    let mut cmd = CommandBuilder::new("cmd");
    cmd.args(["/C", command]);
    cmd
}

// the agent cli to launch, overridable until there is a settings screen
fn agent_command() -> String {
    std::env::var("MUX_AGENT").unwrap_or_else(|_| "claude".to_owned())
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from)
}
