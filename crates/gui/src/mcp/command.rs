use mux_core::protocol::{McpServer, McpTransport};
use serde_json::Value;

pub(super) fn command_line(command: &str, args: &[String], windows: bool) -> String {
    std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .map(|w| quote_for(w, windows))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn summary(server: &McpServer, windows: bool) -> String {
    match &server.transport {
        McpTransport::Stdio { command, args, .. } => command_line(command, args, windows),
        McpTransport::Http { url, .. } => url.clone(),
    }
}

// a word written so `split_for` reads it back exactly. windows splits the way its c runtime does,
// where a backslash only matters before a quote
pub(super) fn quote_for(word: &str, windows: bool) -> String {
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
pub(super) fn split_for(line: &str, windows: bool) -> Vec<String> {
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
pub(super) fn pairs(rows: &[(String, String)]) -> Result<Vec<(String, String)>, String> {
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

pub(super) fn object_pairs(value: &Value) -> Vec<(String, String)> {
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
pub(super) fn imported(text: &str) -> Result<Vec<McpServer>, String> {
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
