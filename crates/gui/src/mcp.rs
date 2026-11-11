use eframe::egui::{self, Align2, CornerRadius, RichText, Sense, pos2, vec2};
use mux_core::Provider;
use mux_core::protocol::{McpServer, McpState, McpTransport};
use mux_ui::theme::{self, font, radius, space, text};
use mux_ui::{Button, IconButton, Segmented, Size, Switch, TextInput, TooltipExt, icons};

use crate::settings::{divider, section, setting};
use serde_json::Value;

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

// well known servers to start from, filled into the form so a path or token can be set before adding
struct Preset {
    name: &'static str,
    label: &'static str,
    detail: &'static str,
    command: Option<&'static str>,
    url: Option<&'static str>,
    env: &'static [&'static str],
    headers: &'static [(&'static str, &'static str)],
}

const PRESETS: &[Preset] = &[
    Preset {
        name: "filesystem",
        label: "Filesystem",
        detail: "Read and write files in folders you pick",
        command: Some("npx -y @modelcontextprotocol/server-filesystem ~/Documents"),
        url: None,
        env: &[],
        headers: &[],
    },
    Preset {
        name: "github",
        label: "GitHub",
        detail: "Issues, pull requests and code search",
        command: None,
        url: Some("https://api.githubcopilot.com/mcp/"),
        env: &[],
        headers: &[("Authorization", "Bearer ")],
    },
    Preset {
        name: "playwright",
        label: "Playwright",
        detail: "Drive a real browser",
        command: Some("npx -y @playwright/mcp@latest"),
        url: None,
        env: &[],
        headers: &[],
    },
    Preset {
        name: "fetch",
        label: "Fetch",
        detail: "Read web pages as text",
        command: Some("uvx mcp-server-fetch"),
        url: None,
        env: &[],
        headers: &[],
    },
    Preset {
        name: "context7",
        label: "Context7",
        detail: "Up to date docs for libraries",
        command: None,
        url: Some("https://mcp.context7.com/mcp"),
        env: &[],
        headers: &[],
    },
];

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

const SHEET: f32 = 520.0;
const PAD: i8 = 20;

// the add, edit and import form in a sheet over the page, with a header and footer like a dialog
fn sheet(ui: &mut egui::Ui, draft: &mut Draft, servers: &mut Vec<McpServer>) {
    let p = theme::palette(ui);
    let editing = draft
        .editing
        .as_ref()
        .and_then(|name| servers.iter().position(|s| &s.name == name));
    let (title, subtitle) = match editing {
        Some(i) => (
            format!("Edit {}", servers[i].name),
            "Agents get the change with their next message",
        ),
        None if draft.json => (
            "Paste JSON".to_owned(),
            "The \"mcpServers\" block most servers show in their README",
        ),
        None => (
            "Add an MCP server".to_owned(),
            "Every agent gets it with their next message",
        ),
    };
    let frame = theme::floating(ui).inner_margin(0);
    let modal = egui::Modal::new(ui.scope_id().with("mcp-sheet"))
        .frame(frame)
        .show(ui.ctx(), |ui| {
            ui.set_width(SHEET);
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::Frame::new()
                .inner_margin(egui::Margin {
                    left: PAD,
                    right: PAD - 6,
                    top: PAD - 2,
                    bottom: 16,
                })
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(
                                RichText::new(&title)
                                    .font(font::semibold(text::MD + 2.0))
                                    .color(p.text),
                            );
                            ui.add_space(space::XS);
                            ui.label(
                                RichText::new(subtitle)
                                    .font(font::regular(text::SM + 1.0))
                                    .color(p.text_faint),
                            );
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if IconButton::new(icons::X, "Close").show(ui).clicked() {
                                draft.open = false;
                            }
                        });
                    });
                });
            hairline(ui);
            egui::Frame::new()
                .inner_margin(egui::Margin::same(PAD))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    if draft.json && editing.is_none() {
                        TextInput::new(&mut draft.paste)
                            .placeholder("{ \"mcpServers\": { \"name\": { \"command\": \"npx\", \"args\": [ … ] } } }")
                            .multiline(8)
                            .show(ui);
                    } else {
                        form(ui, draft);
                    }
                });
            hairline(ui);
            egui::Frame::new()
                .fill(theme::tint(p, theme::fill::CARD))
                .corner_radius(CornerRadius {
                    nw: 0,
                    ne: 0,
                    sw: radius::LG,
                    se: radius::LG,
                })
                .inner_margin(egui::Margin::symmetric(PAD, 14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        error(ui, &draft.error);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let label = match editing {
                                Some(_) => "Save",
                                None if draft.json => "Import",
                                None => "Add server",
                            };
                            if Button::new(label).primary().show(ui).clicked() {
                                save(draft, servers, editing);
                            }
                            if Button::new("Cancel").show(ui).clicked() {
                                draft.open = false;
                            }
                        });
                    });
                });
        });
    if modal.should_close() {
        draft.open = false;
    }
}

fn hairline(ui: &mut egui::Ui) {
    let p = theme::palette(ui);
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
    ui.painter().hline(
        rect.x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, p.border),
    );
}

// adds, saves or imports what the sheet holds, closing it once that worked
fn save(draft: &mut Draft, servers: &mut Vec<McpServer>, editing: Option<usize>) {
    if draft.json && editing.is_none() {
        match imported(&draft.paste) {
            Ok(found) => {
                for server in found {
                    match servers.iter_mut().find(|s| s.name == server.name) {
                        Some(existing) => *existing = server,
                        None => servers.push(server),
                    }
                }
                *draft = Draft::default();
            }
            Err(e) => draft.error = e,
        }
        return;
    }
    let enabled = editing.is_none_or(|i| servers[i].enabled);
    match draft.server(enabled) {
        Ok(server) => {
            let clash = servers
                .iter()
                .enumerate()
                .any(|(i, s)| s.name == server.name && Some(i) != editing);
            if clash {
                draft.error = format!("There's already a server called {}.", server.name);
            } else {
                match editing {
                    Some(i) => servers[i] = server,
                    None => servers.push(server),
                }
                *draft = Draft::default();
            }
        }
        Err(error) => draft.error = error,
    }
}

fn form(ui: &mut egui::Ui, draft: &mut Draft) {
    field(ui, "Name", |ui| {
        TextInput::new(&mut draft.name)
            .placeholder("my-server")
            .show(ui);
    });
    field(ui, "Runs as", |ui| {
        Segmented::new(
            &mut draft.kind,
            &[(Kind::Command, "Command"), (Kind::Url, "URL")],
        )
        .show(ui);
    });
    let reveal = &mut draft.reveal;
    match draft.kind {
        Kind::Command => {
            field(ui, "Command", |ui| {
                TextInput::new(&mut draft.command)
                    .placeholder("npx -y some-mcp-server")
                    .show(ui);
            });
            values(ui, "Environment", &mut draft.env, "API_KEY", reveal);
        }
        Kind::Url => {
            field(ui, "URL", |ui| {
                TextInput::new(&mut draft.url)
                    .placeholder("https://example.com/mcp")
                    .show(ui);
            });
            values(ui, "Headers", &mut draft.headers, "Authorization", reveal);
        }
    }
}

// name and value rows, the values dotted out until shown, since they're usually keys and tokens
fn values(
    ui: &mut egui::Ui,
    label: &str,
    rows: &mut Vec<(String, String)>,
    placeholder: &str,
    reveal: &mut bool,
) {
    let p = theme::palette(ui);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(label)
                .font(font::medium(text::MD))
                .color(p.text),
        );
        if !rows.is_empty() {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (glyph, tip) = if *reveal {
                    (icons::EYE_SLASH, "Hide values")
                } else {
                    (icons::EYE, "Show values")
                };
                if IconButton::new(glyph, "Show values")
                    .size(Size::Sm)
                    .show(ui)
                    .tip(tip)
                    .clicked()
                {
                    *reveal = !*reveal;
                }
            });
        }
    });
    ui.add_space(space::XS);
    let mut removed = None;
    for (i, (key, value)) in rows.iter_mut().enumerate() {
        ui.push_id(i, |ui| {
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let width = ui.available_width() - control_width() - gap * 2.0;
                TextInput::new(key)
                    .placeholder(placeholder)
                    .width(width * 0.4)
                    .show(ui);
                TextInput::new(value)
                    .placeholder("value")
                    .password(!*reveal)
                    .width(width * 0.6)
                    .show(ui);
                if IconButton::new(icons::X, "Remove")
                    .show(ui)
                    .tip("Remove")
                    .clicked()
                {
                    removed = Some(i);
                }
            });
        });
        ui.add_space(space::XS);
    }
    if let Some(i) = removed {
        rows.remove(i);
    }
    let add = if label == "Headers" {
        "Add header"
    } else {
        "Add variable"
    };
    if Button::new(format!("+ {add}"))
        .size(Size::Sm)
        .ghost()
        .show(ui)
        .clicked()
    {
        rows.push((String::new(), String::new()));
        *reveal = true;
    }
    ui.add_space(space::MD);
}

fn control_width() -> f32 {
    mux_ui::theme::control::SM
}

fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui);
    ui.label(
        RichText::new(label)
            .font(font::medium(text::MD))
            .color(p.text),
    );
    ui.add_space(space::SM);
    add(ui);
    ui.add_space(space::LG);
}

fn error(ui: &mut egui::Ui, message: &str) {
    if !message.is_empty() {
        let p = theme::palette(ui);
        ui.add(
            egui::Label::new(
                RichText::new(message)
                    .font(font::regular(text::SM + 1.0))
                    .color(p.danger),
            )
            .wrap(),
        );
    }
}

fn command_line(command: &str, args: &[String], windows: bool) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(|w| quote_for(w, windows))
        .collect::<Vec<_>>()
        .join(" ")
}

fn summary(server: &McpServer, windows: bool) -> String {
    match &server.transport {
        McpTransport::Stdio { command, args, .. } => command_line(command, args, windows),
        McpTransport::Http { url, .. } => url.clone(),
    }
}

// a word written so `split_for` reads it back exactly. windows splits the way its c runtime does,
// where a backslash only matters before a quote
fn quote_for(word: &str, windows: bool) -> String {
    let special: &[char] = if windows { &['"'] } else { &['"', '\'', '\\'] };
    if !word.is_empty() && !word.contains(char::is_whitespace) && !word.contains(special) {
        return word.to_owned();
    }
    let mut quoted = String::from("\"");
    if windows {
        // backslashes are only doubled where a quote follows them, including the closing one
        let mut slashes = 0;
        for c in word.chars() {
            match c {
                '\\' => slashes += 1,
                '"' => {
                    quoted.push_str(&"\\".repeat(slashes * 2 + 1));
                    quoted.push('"');
                    slashes = 0;
                }
                c => {
                    quoted.push_str(&"\\".repeat(slashes));
                    quoted.push(c);
                    slashes = 0;
                }
            }
        }
        quoted.push_str(&"\\".repeat(slashes * 2));
    } else {
        quoted.push_str(&word.replace('\\', "\\\\").replace('"', "\\\""));
    }
    quoted.push('"');
    quoted
}

// a command line split into words the way the server's own system would
fn split_for(line: &str, windows: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote: Option<char> = None;
    let mut chars = line.trim().chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() && quote.is_none() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
                continue;
            }
            '\\' if windows => {
                let mut slashes = 1;
                while chars.peek() == Some(&'\\') {
                    chars.next();
                    slashes += 1;
                }
                if chars.peek() == Some(&'"') {
                    word.push_str(&"\\".repeat(slashes / 2));
                    // an odd run escapes the quote, an even one leaves it to open or close
                    if slashes % 2 == 1 {
                        chars.next();
                        word.push('"');
                    }
                } else {
                    word.push_str(&"\\".repeat(slashes));
                }
            }
            '"' if windows => quote = if quote.is_some() { None } else { Some('"') },
            c if windows => word.push(c),
            '\'' if quote.is_none() => quote = Some('\''),
            '\'' if quote == Some('\'') => quote = None,
            '"' if quote.is_none() => quote = Some('"'),
            '"' if quote == Some('"') => quote = None,
            '\\' if quote != Some('\'') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            c => word.push(c),
        }
        started = true;
    }
    if started {
        words.push(word);
    }
    words
}

// the rows filled in, skipping blank ones, and saying which one is missing its name
fn pairs(rows: &[(String, String)]) -> Result<Vec<(String, String)>, String> {
    rows.iter()
        .map(|(k, v)| (k.trim(), v.trim()))
        .filter(|(k, v)| !k.is_empty() || !v.is_empty())
        .map(|(k, v)| {
            if k.is_empty() {
                Err(format!("Give the value \"{v}\" a name."))
            } else {
                Ok((k.to_owned(), v.to_owned()))
            }
        })
        .collect()
}

fn object_pairs(value: &Value) -> Vec<(String, String)> {
    value
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, v)| {
            (
                k.clone(),
                v.as_str().map_or_else(|| v.to_string(), str::to_owned),
            )
        })
        .collect()
}

// servers from the json a readme shows, keyed by name under mcpServers, servers (vs code's spelling) or at the top
fn imported(text: &str) -> Result<Vec<McpServer>, String> {
    // readmes often show the block on its own, without the braces around it
    let trimmed = text.trim().trim_end_matches(',');
    let value: Value = serde_json::from_str(trimmed)
        .or_else(|_| serde_json::from_str(&format!("{{{trimmed}}}")))
        .map_err(|e| format!("That isn't valid JSON: {e}"))?;
    let map = value
        .get("mcpServers")
        .or(value.get("servers"))
        .unwrap_or(&value)
        .as_object()
        .ok_or("Expected servers keyed by name.")?;
    // codex and opencode only speak streamable http, so an sse server would never connect for them
    let sse: Vec<&str> = map
        .iter()
        .filter(|(_, config)| config["type"] == "sse" || config["transport"] == "sse")
        .map(|(name, _)| name.as_str())
        .collect();
    if !sse.is_empty() {
        return Err(format!(
            "{} uses SSE, which Mux doesn't support yet. Check if the server also has a streamable HTTP URL.",
            sse.join(", ")
        ));
    }
    let servers: Vec<McpServer> = map
        .iter()
        .filter_map(|(name, config)| {
            let transport =
                if let Some(url) = config["url"].as_str().or(config["serverUrl"].as_str()) {
                    McpTransport::Http {
                        url: url.to_owned(),
                        headers: object_pairs(&config["headers"]),
                    }
                } else {
                    McpTransport::Stdio {
                        command: config["command"].as_str()?.to_owned(),
                        args: config["args"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|a| a.as_str().map_or_else(|| a.to_string(), str::to_owned))
                            .collect(),
                        env: object_pairs(&config["env"]),
                    }
                };
            Some(McpServer {
                name: name.clone(),
                enabled: true,
                transport,
            })
        })
        .collect();
    if servers.is_empty() {
        return Err("Didn't find any servers with a command or url in that.".to_owned());
    }
    let bad: Vec<&str> = servers
        .iter()
        .map(|s| s.name.as_str())
        .filter(|n| !McpServer::valid_name(n))
        .collect();
    if !bad.is_empty() {
        return Err(format!(
            "Rename {} first, using only letters, numbers, - and _.",
            bad.join(", ")
        ));
    }
    Ok(servers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_lines_split_like_a_shell() {
        assert_eq!(
            split_for(r#"npx -y "@scope/pkg" --dir 'My Notes' a\ b """#, false),
            ["npx", "-y", "@scope/pkg", "--dir", "My Notes", "a b", ""]
        );
        assert_eq!(
            split_for(
                r#"C:\tools\server.exe "C:\My Files\x" \\host\share say\"hi "C:\dir\\" next it's"#,
                true
            ),
            [
                r"C:\tools\server.exe",
                r"C:\My Files\x",
                r"\\host\share",
                r#"say"hi"#,
                r"C:\dir\",
                "next",
                "it's"
            ]
        );
    }

    #[test]
    fn saved_commands_read_back_word_for_word() {
        let words = [
            "uvx",
            "with space",
            "it's",
            r"a\b",
            r"C:\dir\",
            r"C:\My Files\",
            r#"q"uote"#,
            r#"a\"b"#,
            r"\\host\share",
            "",
            r"trailing\\",
        ]
        .map(str::to_owned);
        for windows in [false, true] {
            let line = words
                .iter()
                .map(|w| quote_for(w, windows))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(
                split_for(&line, windows),
                words,
                "windows: {windows}, line: {line}"
            );
        }
    }

    #[test]
    fn readme_json_imports_in_any_of_its_usual_shapes() {
        let full = r#"{ "mcpServers": {
            "fs": { "command": "npx", "args": ["-y", "server-fs", "/tmp"], "env": { "K": "v" } },
            "web": { "type": "http", "url": "https://x.dev/mcp", "headers": { "Authorization": "Bearer t" } }
        } }"#;
        let servers = imported(full).unwrap();
        assert_eq!(servers.len(), 2);
        assert!(
            matches!(&servers[0].transport, McpTransport::Stdio { args, env, .. }
            if args.len() == 3 && env[0] == ("K".into(), "v".into()))
        );
        assert!(
            matches!(&servers[1].transport, McpTransport::Http { url, .. } if url == "https://x.dev/mcp")
        );
        let bare = r#""fs": { "command": "uvx", "args": ["mcp-fs"] },"#;
        assert_eq!(imported(bare).unwrap()[0].name, "fs");
        assert!(imported("{}").is_err() && imported("nope").is_err());
        let sse = r#"{ "old": { "type": "sse", "url": "https://x.dev/sse" } }"#;
        assert!(imported(sse).unwrap_err().contains("SSE"));
        let spaced = r#"{ "my server": { "command": "x" } }"#;
        assert!(imported(spaced).unwrap_err().contains("my server"));
    }

    #[test]
    fn drafts_check_what_was_typed() {
        let row = |k: &str, v: &str| (k.to_owned(), v.to_owned());
        let draft = Draft {
            name: "fs".into(),
            command: "npx -y server".into(),
            env: vec![row("A", "1"), row("", ""), row(" B ", " two ")],
            ..Default::default()
        };
        let server = draft.server(true).unwrap();
        let McpTransport::Stdio { command, args, env } = &server.transport else {
            panic!("expected a command");
        };
        assert_eq!((command.as_str(), args.len()), ("npx", 2));
        assert_eq!(env.len(), 2, "the blank row is skipped");
        assert_eq!(env[1], ("B".into(), "two".into()));
        let again = Draft::from(&server, false).server(true).unwrap();
        assert_eq!(again, server);
        let nameless = Draft {
            name: "x".into(),
            env: vec![row("", "secret")],
            command: "a".into(),
            ..Default::default()
        };
        assert!(nameless.server(true).is_err());
        let dotted = Draft {
            name: "a.b".into(),
            command: "a".into(),
            ..Default::default()
        };
        assert!(dotted.server(true).is_err());
    }

    #[test]
    fn presets_make_valid_servers() {
        for preset in PRESETS {
            let draft = preset.draft(false);
            assert!(draft.open);
            assert!(draft.server(true).is_ok(), "{}", preset.name);
        }
    }
}
