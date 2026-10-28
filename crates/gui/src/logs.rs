use std::sync::Arc;

use eframe::egui::{self, text::LayoutJob};
use mux_core::Provider;
use mux_core::logging;
use mux_ui::TooltipExt;
use mux_ui::theme::{self, font, space, text};

use crate::settings::{divider, section, setting};

// how much of each log is read, and how many entries are shown and copied
const READ_BYTES: u64 = 256 * 1024;
const SHOWN: usize = 600;

#[derive(Clone)]
struct Entry {
    source: &'static str,
    // the full line as written, with any continuation lines
    text: String,
}

impl Entry {
    fn time(&self) -> &str {
        self.text.get(..24).unwrap_or_default()
    }

    fn level(&self) -> &str {
        self.text.get(25..30).unwrap_or_default().trim()
    }
}

// the settings tab that shows what the app and its server logged, ready to copy into a bug report
pub fn show(ui: &mut egui::Ui, agents: &[(Provider, bool)]) {
    let p = theme::palette(ui);
    let entries = read(ui.ctx());

    section(ui, "Logs", |ui| {
        setting(
            ui,
            "Copy logs",
            "For bug reports. It can include folder paths, so check it first",
            |ui| {
                let copy = mux_ui::Button::new("Copy").size(mux_ui::Size::Sm).show(ui);
                if copy.clicked() {
                    ui.ctx().copy_text(report(&entries, agents));
                    ui.data_mut(|d| d.insert_temp(copy.id.with("copied"), ui.input(|i| i.time)));
                }
                let copied = ui.data(|d| d.get_temp::<f64>(copy.id.with("copied")));
                if copied.is_some_and(|at| ui.input(|i| i.time) - at < 2.0) {
                    ui.label(
                        egui::RichText::new("Copied")
                            .font(font::regular(text::SM + 1.0))
                            .color(p.text_faint),
                    );
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(200));
                }
            },
        );
        divider(ui);
        let folder = mux_core::protocol::data_dir()
            .to_string_lossy()
            .into_owned();
        setting(ui, "Log files", &mux_core::display_path(&folder), |ui| {
            if mux_ui::Button::new("Open")
                .size(mux_ui::Size::Sm)
                .show(ui)
                .tip("Open in the file manager")
                .clicked()
            {
                mux_ui::chat::open_externally(&folder);
            }
        });
    });

    let height = (ui.clip_rect().bottom() - ui.cursor().top() - space::XL * 3.0).max(240.0);
    section(ui, "Recent activity", |ui| {
        ui.add_space(space::MD);
        ui.vertical(|ui| {
            if entries.is_empty() {
                ui.label(
                    egui::RichText::new("Nothing logged yet")
                        .font(font::regular(text::MD))
                        .color(p.text_faint),
                );
                return;
            }
            egui::ScrollArea::vertical()
                .id_salt("logs")
                .max_height(height)
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 3.0;
                    for entry in entries.iter() {
                        ui.label(line(ui, entry));
                    }
                });
        });
        ui.add_space(space::MD);
    });
}

// time, where it came from and the level in quiet colours, so the message is what stands out
fn line(ui: &egui::Ui, entry: &Entry) -> LayoutJob {
    let p = theme::palette(ui);
    let mono = font::mono(text::SM);
    let format = |color| egui::TextFormat::simple(mono.clone(), color);
    let level = match entry.level() {
        "ERROR" => p.danger,
        "WARN" => p.warning,
        _ => p.text_faint,
    };
    let mut job = LayoutJob::default();
    job.wrap.max_width = ui.available_width();
    // the clock is enough to read by, the date is still there when copied
    let clock = entry.time().get(11..19).unwrap_or_default();
    job.append(&format!("{clock} "), 0.0, format(p.text_faint));
    job.append(&format!("{:<6} ", entry.source), 0.0, format(p.text_faint));
    job.append(&format!("{:<5} ", entry.level()), 0.0, format(level));
    // the module that logged it matters to us more than to the person reading, so it waits for the copy
    let rest = entry.text.get(31..).unwrap_or_default();
    let message = rest.split_once(": ").map_or(rest, |(_, message)| message);
    job.append(message, 0.0, format(p.text_muted));
    job
}

// both logs read again at most once a second, merged into one timeline
fn read(ctx: &egui::Context) -> Arc<Vec<Entry>> {
    let id = egui::Id::unique("logs-read");
    let now = ctx.input(|i| i.time);
    if let Some((at, entries)) = ctx.data(|d| d.get_temp::<(f64, Arc<Vec<Entry>>)>(id))
        && now - at < 1.0
    {
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(1.0 - (now - at)));
        return entries;
    }
    let mut entries = Vec::new();
    let files = [("app", logging::path("app.log")), ("server", server_log())];
    for (source, file) in files {
        entries.extend(parse(source, &logging::tail(&file, READ_BYTES)));
    }
    // timestamps lead each line in the same format, so they sort as text
    entries.sort_by(|a, b| a.time().cmp(b.time()));
    let skip = entries.len().saturating_sub(SHOWN);
    let entries = Arc::new(entries.split_off(skip));
    ctx.data_mut(|d| d.insert_temp(id, (now, entries.clone())));
    ctx.request_repaint_after(std::time::Duration::from_secs(1));
    entries
}

// a server in wsl keeps its log in its own data folder, which windows reads through the wsl share
fn server_log() -> std::path::PathBuf {
    #[cfg(windows)]
    if mux_core::paths::wsl().is_some()
        && let Some(folder) = crate::wsl::data_dir()
    {
        return mux_core::paths::local(&format!("{folder}/server.log")).into();
    }
    logging::path("server.log")
}

// an entry starts with its timestamp, and the indented lines after it belong to it
fn parse(source: &'static str, log: &str) -> Vec<Entry> {
    let mut entries: Vec<Entry> = Vec::new();
    for line in log.lines() {
        if line.starts_with(|c: char| c.is_ascii_digit()) {
            entries.push(Entry {
                source,
                text: line.to_owned(),
            });
        } else if let Some(last) = entries.last_mut() {
            last.text.push('\n');
            last.text.push_str(line);
        }
    }
    entries
}

// what gets copied: enough about this machine to make sense of the log, then the log itself
fn report(entries: &[Entry], agents: &[(Provider, bool)]) -> String {
    let found = agents
        .iter()
        .map(|(provider, installed)| {
            let state = if *installed { "found" } else { "not found" };
            format!("{} {state}", provider.label())
        })
        .collect::<Vec<_>>()
        .join(", ");
    let server =
        mux_core::paths::wsl().map_or("beside the app".to_owned(), |d| format!("in WSL ({d})"));
    let mut report = format!(
        "Mux {} ({}) on {} {}\nServer: {server}\nAgents: {found}\n\n",
        mux_core::VERSION,
        crate::update::build(),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    for entry in entries {
        report.push_str(&format!("[{}] {}\n", entry.source, entry.text));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_continuation_lines_with_their_entry() {
        let log = "2026-10-28 12:30:36.123Z ERROR mux::panic: boom\n    at main.rs:1\n2026-10-28 12:30:37.000Z INFO  mux_server: ok\n";
        let entries = parse("server", log);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].level(), "ERROR");
        assert!(entries[0].text.ends_with("at main.rs:1"));
        assert_eq!(entries[1].time(), "2026-10-28 12:30:37.000Z");
        assert_eq!(entries[1].level(), "INFO");
    }
}
