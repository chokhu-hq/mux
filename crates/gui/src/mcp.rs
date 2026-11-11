use eframe::egui::{self, Align2, RichText, Sense, pos2, vec2};
use mux_core::Provider;
use mux_core::protocol::{McpServer, McpState, McpTransport};
use mux_ui::theme::{self, font, space, text};
use mux_ui::{Button, IconButton, Switch, TooltipExt, icons};

use crate::settings::{divider, section, setting};

mod command;
mod presets;
mod sheet;

use command::{command_line, pairs, split_for, summary};
use presets::{PRESETS, Preset};
use sheet::sheet;

#[derive(Clone, Copy, Default, PartialEq)]
enum Kind {
    #[default]
    Command,
    Url,
}

// the server being added or edited, as typed
#[derive(Clone, Default)]
struct Draft {
    // whether the sheet is showing
    open: bool,
    // the name of the server being edited, or none for a new one. a name rather than a place in the list,
    // since the list can change underneath from another window
    editing: Option<String>,
    // split and quoted the way the server's system does, which is linux for a windows app using wsl
    windows: bool,
    // pasting a readme's json instead of filling in the form
    json: bool,
    name: String,
    kind: Kind,
    command: String,
    env: Vec<(String, String)>,
    url: String,
    headers: Vec<(String, String)>,
    paste: String,
    error: String,
    // env and header values show as dots until asked for, since they're usually keys
    reveal: bool,
}

impl Draft {
    fn new(windows: bool) -> Self {
        Draft {
            open: true,
            windows,
            ..Default::default()
        }
    }

    fn from(server: &McpServer, windows: bool) -> Self {
        let mut draft = Draft {
            editing: Some(server.name.clone()),
            name: server.name.clone(),
            ..Draft::new(windows)
        };
        draft.fill(&server.transport);
        draft
    }

    fn fill(&mut self, transport: &McpTransport) {
        match transport {
            McpTransport::Stdio { command, args, env } => {
                self.kind = Kind::Command;
                self.command = command_line(command, args, self.windows);
                self.env = env.clone();
            }
            McpTransport::Http { url, headers } => {
                self.kind = Kind::Url;
                self.url = url.clone();
                self.headers = headers.clone();
            }
        }
    }

    fn server(&self, enabled: bool) -> Result<McpServer, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("Give the server a name.".to_owned());
        }
        if !McpServer::valid_name(name) {
            return Err(
                "Use only letters, numbers, - and _ in the name, since the agents can't take anything else."
                    .to_owned(),
            );
        }
        let transport = match self.kind {
            Kind::Command => {
                let mut words = split_for(&self.command, self.windows).into_iter();
                let Some(command) = words.next() else {
                    return Err("Enter the command that starts the server.".to_owned());
                };
                McpTransport::Stdio {
                    command,
                    args: words.collect(),
                    env: pairs(&self.env)?,
                }
            }
            Kind::Url => {
                let url = self.url.trim();
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err("The URL should start with https://.".to_owned());
                }
                McpTransport::Http {
                    url: url.to_owned(),
                    headers: pairs(&self.headers)?,
                }
            }
        };
        Ok(McpServer {
            name: name.to_owned(),
            enabled,
            transport,
        })
    }
}

impl Preset {
    fn draft(&self, windows: bool) -> Draft {
        let mut draft = Draft {
            name: self.name.to_owned(),
            ..Draft::new(windows)
        };
        if let Some(url) = self.url {
            draft.kind = Kind::Url;
            draft.url = url.to_owned();
        }
        if let Some(command) = self.command {
            draft.command = command.to_owned();
        }
        draft.env = self
            .env
            .iter()
            .map(|k| ((*k).to_owned(), String::new()))
            .collect();
        draft.headers = self
            .headers
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        // a value to fill in is easier with it showing
        draft.reveal = !draft.headers.is_empty() || !draft.env.is_empty();
        draft
    }
}

// the list is none until the server has sent it, since changing it before then would replace what's saved
pub fn show(
    ui: &mut egui::Ui,
    servers: Option<&mut Vec<McpServer>>,
    statuses: &[(String, Provider, McpState)],
    windows: bool,
) {
    let p = theme::palette(ui);
    let Some(servers) = servers else {
        ui.label(
            RichText::new("Waiting for the Mux server…")
                .font(font::regular(text::MD))
                .color(p.text_faint),
        );
        return;
    };
    let id = ui.make_persistent_id("mcp-draft");
    let mut draft: Draft = ui.data(|d| d.get_temp(id)).unwrap_or_default();
    draft.windows = windows;

    ui.add(
        egui::Label::new(
            RichText::new(
                "Mux gives these servers to every Claude, Codex and OpenCode agent, alongside the ones each CLI already has set up. An agent picks up a change with its next message.",
            )
            .font(font::regular(text::SM + 1.0))
            .color(p.text_faint),
        )
        .wrap(),
    );
    ui.add_space(space::LG);

    section(ui, "Servers", |ui| {
        let mut removed = None;
        for (i, server) in servers.iter_mut().enumerate() {
            ui.push_id(i, |ui| match row(ui, server, statuses, windows) {
                Some(Row::Edit) => draft = Draft::from(server, windows),
                Some(Row::Remove) => removed = Some(i),
                None => {}
            });
            divider(ui);
        }
        if let Some(i) = removed {
            let gone = servers.remove(i);
            if draft.editing.as_ref() == Some(&gone.name) {
                draft = Draft::default();
            }
        }
        let (title, detail) = if servers.is_empty() {
            (
                "No servers yet",
                "MCP servers give agents new tools, like a browser or your GitHub",
            )
        } else {
            ("Add a server", "From the command or URL in its README")
        };
        setting(ui, title, detail, |ui| {
            if Button::new("Add server").primary().show(ui).clicked() {
                draft = Draft::new(windows);
            }
            if Button::new("Paste JSON").show(ui).clicked() {
                draft = Draft {
                    json: true,
                    ..Draft::new(windows)
                };
            }
        });
    });

    // the well known servers not added yet, each a click from a filled in form
    let popular: Vec<&Preset> = PRESETS
        .iter()
        .filter(|preset| servers.iter().all(|s| s.name != preset.name))
        .collect();
    if !popular.is_empty() {
        section(ui, "Popular servers", |ui| {
            for (i, preset) in popular.iter().enumerate() {
                if i > 0 {
                    divider(ui);
                }
                ui.push_id(preset.name, |ui| {
                    setting(ui, preset.label, preset.detail, |ui| {
                        if Button::new("Add").show(ui).clicked() {
                            draft = preset.draft(windows);
                        }
                    });
                });
            }
        });
    }

    if draft.open {
        sheet(ui, &mut draft, servers);
    }
    ui.data_mut(|d| d.insert_temp(id, draft));
}

enum Row {
    Edit,
    Remove,
}

// a server like any other setting row, with how it runs, how it's doing, its switch and its buttons
fn row(
    ui: &mut egui::Ui,
    server: &mut McpServer,
    statuses: &[(String, Provider, McpState)],
    windows: bool,
) -> Option<Row> {
    let p = theme::palette(ui);
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 60.0), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, true, &server.name));
    let mut picked = response.clicked().then_some(Row::Edit);
    let response = response.on_hover_cursor(egui::CursorIcon::PointingHand);

    // the controls first, so the text knows how much room it has
    let mut controls = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    if IconButton::new(icons::TRASH, "Remove")
        .show(&mut controls)
        .tip("Remove")
        .clicked()
    {
        picked = Some(Row::Remove);
    }
    if IconButton::new(icons::PENCIL_SIMPLE, "Edit")
        .show(&mut controls)
        .tip("Edit")
        .clicked()
    {
        picked = Some(Row::Edit);
    }
    controls.add_space(space::SM);
    let label = format!("{} on", server.name);
    Switch::new(&mut server.enabled)
        .label(&label)
        .show(&mut controls)
        .tip(if server.enabled {
            "On for every agent"
        } else {
            "Off, so agents don't get it"
        });
    let room = controls.min_rect().left() - space::LG;

    let x = rect.left();
    let width = (room - x).max(0.0);
    let line = |ui: &egui::Ui, text: String, font, color| {
        let mut job = egui::text::LayoutJob::simple_singleline(text, font, color);
        job.wrap = egui::text::TextWrapping::truncate_at_width(width);
        ui.painter().layout_job(job)
    };
    let name = line(
        ui,
        server.name.clone(),
        font::medium(text::MD),
        if server.enabled { p.text } else { p.text_muted },
    );
    let name_rect = egui::Rect::from_min_size(pos2(x, rect.center().y - 18.0), name.size());
    ui.painter().galley(name_rect.min, name, p.text);
    let (color, label, tip) = status(server, statuses, p);
    let dot = pos2(name_rect.right() + space::MD, name_rect.center().y);
    if dot.x + 80.0 < room {
        ui.painter().circle_filled(dot, 3.5, color);
        let shown = ui.painter().text(
            pos2(dot.x + space::SM, dot.y),
            Align2::LEFT_CENTER,
            label,
            font::regular(text::SM + 1.0),
            p.text_muted,
        );
        ui.interact(
            shown.union(egui::Rect::from_center_size(dot, vec2(8.0, 8.0))),
            response.id.with("status"),
            Sense::hover(),
        )
        .tip(tip);
    }
    let detail = line(
        ui,
        summary(server, windows),
        font::regular(text::SM + 1.0),
        p.text_faint,
    );
    ui.painter()
        .galley(pos2(x, rect.center().y + 2.0), detail, p.text_faint);
    picked
}

// how the server last started, worst first, with which cli said so a hover away
fn status(
    server: &McpServer,
    statuses: &[(String, Provider, McpState)],
    p: &theme::Palette,
) -> (egui::Color32, &'static str, String) {
    if !server.enabled {
        return (
            p.text_faint,
            "Off",
            "Agents won't get this server".to_owned(),
        );
    }
    let mine: Vec<(Provider, &McpState)> = statuses
        .iter()
        .filter(|(n, ..)| *n == server.name)
        .map(|(_, provider, state)| (*provider, state))
        .collect();
    let failed: Vec<String> = mine
        .iter()
        .filter_map(|(provider, state)| match state {
            McpState::Failed(reason) if reason.is_empty() => {
                Some(format!("{} couldn't start it", provider.label()))
            }
            McpState::Failed(reason) => Some(format!("{}: {reason}", provider.label())),
            _ => None,
        })
        .collect();
    let connected: Vec<&str> = mine
        .iter()
        .filter(|(_, state)| **state == McpState::Connected)
        .map(|(provider, _)| provider.label())
        .collect();
    if !failed.is_empty() {
        (p.danger, "Failed", failed.join("\n"))
    } else if !connected.is_empty() {
        let tip = format!("Connected in {}", connected.join(", "));
        (p.success, "Connected", tip)
    } else if !mine.is_empty() {
        (p.warning, "Starting", "Starting up".to_owned())
    } else {
        (
            p.text_faint,
            "Not used yet",
            "It starts with an agent's next message".to_owned(),
        )
    }
}

#[cfg(test)]
mod tests;
