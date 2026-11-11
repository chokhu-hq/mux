use eframe::egui::{self, RichText};
use mux_core::protocol::{McpServer, McpTransport};
use mux_ui::theme::{self, font, space, text};
use mux_ui::{Button, IconButton, Segmented, TextInput, TooltipExt, icons};
use serde_json::Value;

use crate::settings::{divider, section, setting};

#[derive(Clone, Copy, Default, PartialEq)]
enum Kind {
    #[default]
    Command,
    Url,
}

// the server being added or edited, as typed
#[derive(Clone, Default)]
struct Draft {
    // the server being edited, or none for a new one
    editing: Option<usize>,
    name: String,
    kind: Kind,
    command: String,
    env: String,
    url: String,
    headers: String,
    paste: String,
    error: String,
    // a saved server's env and header values stay hidden until the box is clicked
    hidden: bool,
}

impl Draft {
    fn from(index: usize, server: &McpServer) -> Self {
        let mut draft = Draft {
            editing: Some(index),
            name: server.name.clone(),
            hidden: true,
            ..Default::default()
        };
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                draft.command = std::iter::once(command)
                    .chain(args)
                    .map(|w| quote(w))
                    .collect::<Vec<_>>()
                    .join(" ");
                draft.env = lines(env, "=");
            }
            McpTransport::Http { url, headers } => {
                draft.kind = Kind::Url;
                draft.url = url.clone();
                draft.headers = lines(headers, ": ");
            }
        }
        draft
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
                let mut words = split(&self.command).into_iter();
                let Some(command) = words.next() else {
                    return Err("Enter the command that starts the server.".to_owned());
                };
                McpTransport::Stdio {
                    command,
                    args: words.collect(),
                    env: pairs(&self.env, '=')?,
                }
            }
            Kind::Url => {
                let url = self.url.trim();
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err("The URL should start with https://.".to_owned());
                }
                McpTransport::Http {
                    url: url.to_owned(),
                    headers: pairs(&self.headers, ':')?,
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

pub fn show(ui: &mut egui::Ui, servers: &mut Vec<McpServer>) {
    let p = theme::palette(ui);
    let id = ui.make_persistent_id("mcp-draft");
    let mut draft: Draft = ui.data(|d| d.get_temp(id)).unwrap_or_default();

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
        if servers.is_empty() {
            setting(
                ui,
                "No servers yet",
                "Add one below, or paste the JSON from a server's README",
                |_| {},
            );
        }
        let mut removed = None;
        for (i, server) in servers.iter_mut().enumerate() {
            if i > 0 {
                divider(ui);
            }
            ui.push_id(i, |ui| {
                let (name, detail) = (server.name.clone(), summary(server));
                setting(ui, &name, &detail, |ui| {
                    if IconButton::new(icons::TRASH, "Remove")
                        .show(ui)
                        .tip("Remove")
                        .clicked()
                    {
                        removed = Some(i);
                    }
                    if IconButton::new(icons::PENCIL_SIMPLE, "Edit")
                        .show(ui)
                        .tip("Edit")
                        .clicked()
                    {
                        draft = Draft::from(i, server);
                    }
                    ui.add_space(space::SM);
                    Segmented::new(&mut server.enabled, &[(true, "On"), (false, "Off")]).show(ui);
                });
            });
        }
        if let Some(i) = removed {
            servers.remove(i);
            draft.editing = draft
                .editing
                .filter(|&e| e != i)
                .map(|e| e - usize::from(e > i));
        }
    });

    let title = match draft.editing.and_then(|i| servers.get(i)) {
        Some(server) => format!("Edit {}", server.name),
        None => "Add a server".to_owned(),
    };
    section(ui, &title, |ui| {
        ui.add_space(space::LG);
        field(ui, "Name", |ui| {
            TextInput::new(&mut draft.name)
                .placeholder("filesystem")
                .show(ui);
        });
        field(ui, "Runs as", |ui| {
            Segmented::new(
                &mut draft.kind,
                &[(Kind::Command, "Command"), (Kind::Url, "URL")],
            )
            .show(ui);
        });
        match draft.kind {
            Kind::Command => {
                field(ui, "Command", |ui| {
                    TextInput::new(&mut draft.command)
                        .placeholder("npx -y @modelcontextprotocol/server-filesystem ~/notes")
                        .show(ui);
                });
                field(ui, "Environment, one KEY=value per line", |ui| {
                    secret(ui, &mut draft.env, '=', "API_KEY=…", &mut draft.hidden);
                });
            }
            Kind::Url => {
                field(ui, "URL", |ui| {
                    TextInput::new(&mut draft.url)
                        .placeholder("https://example.com/mcp")
                        .show(ui);
                });
                field(ui, "Headers, one Name: value per line", |ui| {
                    secret(
                        ui,
                        &mut draft.headers,
                        ':',
                        "Authorization: Bearer …",
                        &mut draft.hidden,
                    );
                });
            }
        }
        ui.horizontal(|ui| {
            let editing = draft.editing.filter(|&i| i < servers.len());
            let label = if editing.is_some() {
                "Save"
            } else {
                "Add server"
            };
            if Button::new(label).primary().show(ui).clicked() {
                let enabled = editing.is_none_or(|i| servers[i].enabled);
                match draft.server(enabled) {
                    Ok(server) => {
                        let clash = servers
                            .iter()
                            .enumerate()
                            .any(|(i, s)| s.name == server.name && Some(i) != editing);
                        if clash {
                            draft.error =
                                format!("There's already a server called {}.", server.name);
                        } else {
                            match editing {
                                Some(i) => servers[i] = server,
                                None => servers.push(server),
                            }
                            draft = Draft::default();
                        }
                    }
                    Err(error) => draft.error = error,
                }
            }
            if editing.is_some() && Button::new("Cancel").ghost().show(ui).clicked() {
                draft = Draft::default();
            }
            error(ui, &draft.error);
        });
        ui.add_space(space::LG);
    });

    section(ui, "Paste JSON", |ui| {
        ui.add_space(space::LG);
        field(
            ui,
            "The \"mcpServers\" block most servers show in their README",
            |ui| {
                TextInput::new(&mut draft.paste)
                    .placeholder("{ \"mcpServers\": { \"name\": { \"command\": \"npx\", \"args\": [ … ] } } }")
                    .multiline(4)
                    .show(ui);
            },
        );
        ui.horizontal(|ui| {
            if Button::new("Import").show(ui).clicked() {
                match imported(&draft.paste) {
                    Ok(found) => {
                        for server in found {
                            match servers.iter_mut().find(|s| s.name == server.name) {
                                Some(existing) => *existing = server,
                                None => servers.push(server),
                            }
                        }
                        draft.paste.clear();
                        draft.error.clear();
                    }
                    Err(e) => draft.error = e,
                }
            }
            if draft.editing.is_none() {
                error(ui, &draft.error);
            }
        });
        ui.add_space(space::LG);
    });

    ui.data_mut(|d| d.insert_temp(id, draft));
}

fn field(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) {
    let p = theme::palette(ui);
    ui.label(
        RichText::new(label)
            .font(font::medium(text::SM + 1.0))
            .color(p.text_muted),
    );
    ui.add_space(space::XS);
    add(ui);
    ui.add_space(space::MD);
}

// name and value lines with the values masked while `hidden`, shown once the box is clicked
fn secret(
    ui: &mut egui::Ui,
    text: &mut String,
    between: char,
    placeholder: &str,
    hidden: &mut bool,
) {
    if !*hidden || text.trim().is_empty() {
        TextInput::new(text)
            .placeholder(placeholder)
            .multiline(2)
            .show(ui);
        return;
    }
    let mut masked = masked(text, between);
    let response = TextInput::new(&mut masked)
        .multiline(2)
        .show(ui)
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .tip("Click to show the values");
    if response.clicked() || response.has_focus() {
        *hidden = false;
    }
}

fn masked(text: &str, between: char) -> String {
    text.lines()
        .map(|line| match line.split_once(between) {
            Some((name, _)) if between == ':' => format!("{name}: ••••••••"),
            Some((name, _)) => format!("{name}{between}••••••••"),
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn error(ui: &mut egui::Ui, message: &str) {
    if !message.is_empty() {
        let p = theme::palette(ui);
        ui.add_space(space::SM);
        ui.label(
            RichText::new(message)
                .font(font::regular(text::SM + 1.0))
                .color(p.danger),
        );
    }
}

fn summary(server: &McpServer) -> String {
    match &server.transport {
        McpTransport::Stdio { command, args, .. } => std::iter::once(command)
            .chain(args)
            .map(|w| quote(w))
            .collect::<Vec<_>>()
            .join(" "),
        McpTransport::Http { url, .. } => url.clone(),
    }
}

// windows paths are full of backslashes, so there a backslash only escapes a quote
const WINDOWS: bool = cfg!(windows);

fn quote(word: &str) -> String {
    quote_for(word, WINDOWS)
}

fn quote_for(word: &str, windows: bool) -> String {
    if word.is_empty() || word.contains(char::is_whitespace) || word.contains('"') {
        let word = if windows {
            word.to_owned()
        } else {
            word.replace('\\', "\\\\")
        };
        format!("\"{}\"", word.replace('"', "\\\""))
    } else {
        word.to_owned()
    }
}

fn lines(list: &[(String, String)], between: &str) -> String {
    list.iter()
        .map(|(k, v)| format!("{k}{between}{v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// a command line split into words the way a shell would, honouring quotes
fn split(line: &str) -> Vec<String> {
    split_for(line, WINDOWS)
}

fn split_for(line: &str, windows: bool) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut started = false;
    let mut quote = None;
    let mut chars = line.trim().chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, c) if c.is_whitespace() => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => word.push(c),
            (_, '\\') if windows && !chars.as_str().starts_with('"') => {
                word.push('\\');
                started = true;
            }
            (_, '\\') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
                started = true;
            }
            (_, c) => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

fn pairs(text: &str, between: char) -> Result<Vec<(String, String)>, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| match l.split_once(between) {
            Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_owned(), v.trim().to_owned())),
            _ => Err(format!(
                "Couldn't read \"{l}\", expected name{between}value."
            )),
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
                r#"C:\tools\server.exe "C:\My Files\x" \\host\share say\"hi"#,
                true
            ),
            [
                r"C:\tools\server.exe",
                r"C:\My Files\x",
                r"\\host\share",
                r#"say"hi"#
            ]
        );
        for windows in [false, true] {
            let words = ["uvx", "with space", r"C:\a b\c", r#"q"uote"#, ""].map(str::to_owned);
            let line = words
                .iter()
                .map(|w| quote_for(w, windows))
                .collect::<Vec<_>>()
                .join(" ");
            assert_eq!(split_for(&line, windows), words, "windows: {windows}");
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
        let spaced = r#"{ "my server": { "command": "x" } }"#;
        assert!(imported(spaced).unwrap_err().contains("my server"));
    }

    #[test]
    fn drafts_check_what_was_typed() {
        let draft = Draft {
            name: "fs".into(),
            command: "npx -y server".into(),
            env: "A=1\n\nB = two".into(),
            ..Default::default()
        };
        let server = draft.server(true).unwrap();
        let McpTransport::Stdio { command, args, env } = &server.transport else {
            panic!("expected a command");
        };
        assert_eq!((command.as_str(), args.len()), ("npx", 2));
        assert_eq!(env[1], ("B".into(), "two".into()));
        let again = Draft::from(0, &server).server(true).unwrap();
        assert_eq!(
            masked("A=1\nAuthorization: Bearer t", ':'),
            "A=1\nAuthorization: ••••••••"
        );
        assert_eq!(masked("API_KEY=secret", '='), "API_KEY=••••••••");
        assert_eq!(again, server);
        let bad = Draft {
            name: "x".into(),
            env: "nothing".into(),
            command: "a".into(),
            ..Default::default()
        };
        assert!(bad.server(true).is_err());
        let dotted = Draft {
            name: "a.b".into(),
            command: "a".into(),
            ..Default::default()
        };
        assert!(dotted.server(true).is_err());
    }
}
