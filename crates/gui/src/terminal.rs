use std::sync::mpsc::{self, Receiver, Sender};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use eframe::egui;
use mux_core::protocol::ClientMsg;
use mux_ui::TerminalView;

#[derive(Clone)]
struct Listener(Sender<Event>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let _ = self.0.send(event);
    }
}

// a local copy of a server-side terminal, rebuilt from the replay and kept current from output
pub struct Terminal {
    term: FairMutex<Term<Listener>>,
    parser: Processor,
    events: Receiver<Event>,
}

impl Terminal {
    pub fn new(cols: u16, rows: u16, replay: &[u8]) -> Self {
        let (tx, events) = mpsc::channel();
        let term = Term::new(
            Config::default(),
            &TermSize::new(cols.max(1) as usize, rows.max(1) as usize),
            Listener(tx),
        );
        let mut terminal = Self {
            term: FairMutex::new(term),
            parser: Processor::new(),
            events,
        };
        terminal.feed(replay);
        terminal
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.parser.advance(&mut *self.term.lock(), data);
    }

    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        id: u64,
        outbox: &mut Vec<ClientMsg>,
        size: f32,
    ) -> (egui::Response, Option<f32>) {
        // the server answers terminal queries, so only clipboard writes matter here
        while let Ok(event) = self.events.try_recv() {
            if let Event::ClipboardStore(_, text) = event {
                ui.ctx().copy_text(text);
            }
        }
        let out = TerminalView::new(&self.term).font_size(size).show(ui);
        if !out.input.is_empty() {
            outbox.push(ClientMsg::Input {
                id,
                data: out.input,
            });
        }
        if let Some(size) = out.resize {
            outbox.push(ClientMsg::Resize {
                id,
                cols: size.num_cols,
                rows: size.num_lines,
            });
        }
        (out.response, out.zoom)
    }
}
