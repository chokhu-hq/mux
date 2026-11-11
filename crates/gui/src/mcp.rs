use eframe::egui::{self, RichText};
use mux_core::Provider;
use mux_core::protocol::{McpServer, McpState, McpTransport};
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
    // the name of the server being edited, or none for a new one. a name rather than a place in the list,
    // since the list can change underneath from another window
    editing: Option<String>,
    // split and quoted the way the server's system does, which is linux for a windows app using wsl
    windows: bool,
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
    fn from(server: &McpServer, windows: bool) -> Self {
        let mut draft = Draft {
            editing: Some(server.name.clone()),
            windows,
            name: server.name.clone(),
            hidden: true,
            ..Default::default()
        };
        match &server.transport {
            McpTransport::Stdio { command, args, env } => {
                draft.command = command_line(command, args, windows);
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
                let mut words = split_for(&self.command, self.windows).into_iter();
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
                let (name, detail) = (server.name.clone(), summary(server, windows));
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
                        draft = Draft::from(server, windows);
                    }
                    ui.add_space(space::SM);
                    Segmented::new(&mut server.enabled, &[(true, "On"), (false, "Off")]).show(ui);
                    if server.enabled {
                        ui.add_space(space::SM);
                        status(ui, &name, statuses);
                    }
                });
            });
        }
        if let Some(i) = removed {
            let gone = servers.remove(i);
            if draft.editing.as_ref() == Some(&gone.name) {
                draft = Draft::default();
            }
        }
    });

    // the server being edited, if it's still there
    let editing = draft
        .editing
        .as_ref()
        .and_then(|name| servers.iter().position(|s| &s.name == name));
    let title = match editing {
        Some(i) => format!("Edit {}", servers[i].name),
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
                            draft = Draft {
                                windows,
                                ..Default::default()
                            };
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

// how the server last started, worst first, with which cli said so a hover away
fn status(ui: &mut egui::Ui, name: &str, statuses: &[(String, Provider, McpState)]) {
    let p = theme::palette(ui);
    let mine: Vec<(Provider, &McpState)> = statuses
        .iter()
        .filter(|(n, ..)| n == name)
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
    let (glyph, label, color, tip) = if !failed.is_empty() {
        (
            icons::WARNING_CIRCLE,
            "Failed",
            p.warning,
            failed.join("\n"),
        )
    } else if !connected.is_empty() {
        let tip = format!("Connected in {}", connected.join(", "));
        (icons::CHECK_CIRCLE, "Connected", p.success, tip)
    } else if !mine.is_empty() {
        (
            icons::CLOCK,
            "Starting",
            p.text_faint,
            "Starting up".to_owned(),
        )
    } else {
        return;
    };
    let shown = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::XS;
            ui.label(
                RichText::new(label)
                    .font(font::regular(text::SM + 1.0))
                    .color(p.text_muted),
            );
            ui.label(
                RichText::new(glyph.to_string())
                    .font(mux_ui::theme::font::icon(mux_ui::theme::icon::MD))
                    .color(color),
            );
        })
        .response;
    shown.tip(tip);
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

fn lines(list: &[(String, String)], between: &str) -> String {
    list.iter()
        .map(|(k, v)| format!("{k}{between}{v}"))
        .collect::<Vec<_>>()
        .join("\n")
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
        let again = Draft::from(&server, false).server(true).unwrap();
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
