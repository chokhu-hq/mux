use std::io;
use std::path::{Path, PathBuf};

use mux_core::protocol::{McpServer, McpTransport, Question, QuestionOption};
use serde_json::{Map, Value, json};

// claude's servers go in a file rather than on its command line, where any process could read the secrets
pub fn claude_file(servers: &[McpServer], session: u64) -> io::Result<Option<PathBuf>> {
    claude_file_in(&folder(), servers, session)
}

// removes a session's claude config, once the session is gone
pub fn forget_claude(session: u64) {
    forget_claude_in(&folder(), session);
}

// removes the claude configs of sessions not running, when the servers change, since each is rewritten with the
// new list the next time its claude starts. a running claude may still be about to read its own
pub fn forget_idle_claude(running: &[u64]) {
    forget_idle_in(&folder(), running);
}

// pictures in a tool's result, saved privately so the chat can show them, in either shape the clis report:
// claude's `source` block or mcp's own `data` and `mimeType`
pub fn save_images(content: &Value, session: u64) -> Vec<String> {
    save_images_in(&folder().join("images"), content, session)
}

fn save_images_in(dir: &Path, content: &Value, session: u64) -> Vec<String> {
    use base64::Engine;
    let mut saved = Vec::new();
    for part in content.as_array().into_iter().flatten() {
        if part["type"] != "image" {
            continue;
        }
        let (data, mime) = match part["source"]["data"].as_str() {
            Some(data) => (data, part["source"]["media_type"].as_str()),
            None => match part["data"].as_str() {
                Some(data) => (data, part["mimeType"].as_str()),
                None => continue,
            },
        };
        let extension = match mime.unwrap_or_default() {
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        };
        let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
            continue;
        };
        if mux_core::private_dir(dir).is_err() {
            break;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let path = dir.join(format!("{session}-{stamp}-{}.{extension}", saved.len()));
        if mux_core::write_private(&path, &bytes).is_ok() {
            saved.push(path.to_string_lossy().into_owned());
        }
    }
    saved
}

// claude saves a tool's pictures itself and leaves `[Image: source: <path>]` in the text, so those lines become
// pictures and drop out of the text
pub fn image_refs(text: &str) -> (String, Vec<String>) {
    let mut kept = Vec::new();
    let mut paths = Vec::new();
    for line in text.lines() {
        let path = line
            .trim()
            .strip_prefix("[Image: source: ")
            .and_then(|rest| rest.strip_suffix(']'))
            .filter(|p| Path::new(p).is_absolute() && mux_core::protocol::is_image(p));
        match path {
            Some(path) => paths.push(path.to_owned()),
            None => kept.push(line),
        }
    }
    (kept.join("\n"), paths)
}

// a deleted session's pictures go with it
pub fn forget_images(session: u64) {
    let prefix = format!("{session}-");
    for entry in std::fs::read_dir(folder().join("images"))
        .into_iter()
        .flatten()
        .flatten()
    {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn folder() -> PathBuf {
    mux_core::protocol::data_dir().join("mcp")
}

fn claude_file_in(dir: &Path, servers: &[McpServer], session: u64) -> io::Result<Option<PathBuf>> {
    let Some(config) = claude(servers) else {
        // keys from servers since turned off or removed don't stay behind
        forget_claude_in(dir, session);
        return Ok(None);
    };
    mux_core::private_dir(dir)?;
    let path = dir.join(format!("claude-{session}.json"));
    mux_core::write_private(&path, config.as_bytes())?;
    Ok(Some(path))
}

fn forget_claude_in(dir: &Path, session: u64) {
    let _ = std::fs::remove_file(dir.join(format!("claude-{session}.json")));
}

fn forget_idle_in(dir: &Path, running: &[u64]) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let session = name.to_str().and_then(|n| {
            n.strip_prefix("claude-")?
                .strip_suffix(".json")?
                .parse::<u64>()
                .ok()
        });
        if session.is_some_and(|s| !running.contains(&s)) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn pairs(list: &[(String, String)]) -> Value {
    Value::Object(
        list.iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect::<Map<_, _>>(),
    )
}

fn enabled(servers: &[McpServer]) -> impl Iterator<Item = &McpServer> {
    servers
        .iter()
        .filter(|s| s.enabled && McpServer::valid_name(&s.name))
}

// claude's --mcp-config, or none when there's nothing to add
pub fn claude(servers: &[McpServer]) -> Option<String> {
    let map: Map<String, Value> = enabled(servers)
        .map(|s| {
            let config = match &s.transport {
                McpTransport::Stdio { command, args, env } => {
                    json!({ "command": command, "args": args, "env": pairs(env) })
                }
                McpTransport::Http { url, headers } => {
                    json!({ "type": "http", "url": url, "headers": pairs(headers) })
                }
            };
            (s.name.clone(), config)
        })
        .collect();
    (!map.is_empty()).then(|| json!({ "mcpServers": map }).to_string())
}

// codex's `mcp_servers` config table, passed when its thread opens
pub fn codex(servers: &[McpServer]) -> Option<Value> {
    let map: Map<String, Value> = enabled(servers)
        .map(|s| {
            let config = match &s.transport {
                McpTransport::Stdio { command, args, env } => {
                    json!({ "command": command, "args": args, "env": pairs(env) })
                }
                McpTransport::Http { url, headers } => {
                    json!({ "url": url, "http_headers": pairs(headers) })
                }
            };
            (s.name.clone(), config)
        })
        .collect();
    (!map.is_empty()).then_some(Value::Object(map))
}

// the acp session's `mcpServers` list, for opencode
pub fn acp(servers: &[McpServer]) -> Value {
    let named = |list: &[(String, String)]| -> Vec<Value> {
        list.iter()
            .map(|(k, v)| json!({ "name": k, "value": v }))
            .collect()
    };
    enabled(servers)
        .map(|s| match &s.transport {
            McpTransport::Stdio { command, args, env } => {
                json!({ "name": s.name, "command": command, "args": args, "env": named(env) })
            }
            McpTransport::Http { url, headers } => {
                json!({ "type": "http", "name": s.name, "url": url, "headers": named(headers) })
            }
        })
        .collect()
}

// the choices of an enum field as (value sent back, label shown)
fn choices(field: &Value) -> Vec<(String, String)> {
    let titled = field["oneOf"]
        .as_array()
        .or(field["anyOf"].as_array())
        .or(field["items"]["anyOf"].as_array())
        .or(field["items"]["oneOf"].as_array());
    if let Some(titled) = titled {
        return titled
            .iter()
            .filter_map(|o| {
                let value = o["const"].as_str()?.to_owned();
                let label = o["title"].as_str().unwrap_or(&value).to_owned();
                Some((value, label))
            })
            .collect();
    }
    let values = field["enum"]
        .as_array()
        .or(field["items"]["enum"].as_array());
    let names = field["enumNames"].as_array();
    values
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(i, v)| {
            let value = v.as_str()?.to_owned();
            let label = names
                .and_then(|n| n.get(i))
                .and_then(Value::as_str)
                .unwrap_or(&value)
                .to_owned();
            Some((value, label))
        })
        .collect()
}

// one question per field of an mcp form, in the order the server listed them
pub fn questions(schema: &Value, order: &[String]) -> Vec<Question> {
    let Some(fields) = schema["properties"].as_object() else {
        return Vec::new();
    };
    let required: Vec<&str> = schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut keys: Vec<&String> = order.iter().filter(|k| fields.contains_key(*k)).collect();
    keys.extend(fields.keys().filter(|k| !order.contains(k)));
    keys.into_iter()
        .map(|key| {
            let field = &fields[key];
            let title = field["title"].as_str().unwrap_or(key);
            let description = field["description"].as_str().unwrap_or_default();
            let options: Vec<QuestionOption> = if field["type"] == "boolean" {
                ["Yes", "No"]
                    .map(|label| QuestionOption {
                        label: label.to_owned(),
                        description: String::new(),
                    })
                    .into()
            } else {
                choices(field)
                    .into_iter()
                    .map(|(_, label)| QuestionOption {
                        label,
                        description: String::new(),
                    })
                    .collect()
            };
            Question {
                id: key.clone(),
                header: if description.is_empty() {
                    String::new()
                } else {
                    title.to_owned()
                },
                question: if description.is_empty() {
                    title.to_owned()
                } else {
                    description.to_owned()
                },
                other: options.is_empty(),
                multiple: field["type"] == "array",
                optional: !required.contains(&key.as_str()),
                options,
            }
        })
        .collect()
}

// the form's answers typed the way its schema asks, leaving out anything left blank
pub fn content(schema: &Value, ids: &[String], answers: &[Vec<String>]) -> Value {
    let mut content = Map::new();
    for (key, picks) in ids.iter().zip(answers) {
        let field = &schema["properties"][key];
        let Some(first) = picks.first().filter(|_| field.is_object()) else {
            continue;
        };
        let pick_value = |label: &String| {
            choices(field)
                .into_iter()
                .find(|(_, l)| l == label)
                .map_or_else(|| label.clone(), |(value, _)| value)
        };
        let value = match field["type"].as_str() {
            Some("boolean") => json!(first == "Yes"),
            Some("integer") => first
                .trim()
                .parse::<i64>()
                .map_or_else(|_| json!(first), |n| json!(n)),
            Some("number") => first
                .trim()
                .parse::<f64>()
                .map_or_else(|_| json!(first), |n| json!(n)),
            Some("array") => json!(picks.iter().map(pick_value).collect::<Vec<_>>()),
            _ => json!(pick_value(first)),
        };
        content.insert(key.clone(), value);
    }
    Value::Object(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(name: &str) -> McpServer {
        McpServer {
            name: name.to_owned(),
            enabled: true,
            transport: McpTransport::Stdio {
                command: "python3".to_owned(),
                args: vec!["server.py".to_owned()],
                env: vec![("KEY".to_owned(), "1".to_owned())],
            },
        }
    }

    #[test]
    fn each_cli_gets_the_enabled_servers_in_its_own_shape() {
        let web = McpServer {
            name: "web".to_owned(),
            enabled: true,
            transport: McpTransport::Http {
                url: "https://x.dev/mcp".to_owned(),
                headers: vec![("Authorization".to_owned(), "Bearer t".to_owned())],
            },
        };
        let mut off = stdio("off");
        off.enabled = false;
        let servers = [stdio("probe"), web, off, stdio("bad name")];
        let claude: Value = serde_json::from_str(&super::claude(&servers).unwrap()).unwrap();
        assert_eq!(claude["mcpServers"]["probe"]["env"]["KEY"], "1");
        assert_eq!(claude["mcpServers"]["web"]["type"], "http");
        assert!(claude["mcpServers"].get("off").is_none());
        let codex = super::codex(&servers).unwrap();
        assert_eq!(codex["web"]["http_headers"]["Authorization"], "Bearer t");
        let acp = super::acp(&servers);
        assert_eq!(acp[0]["env"][0]["name"], "KEY");
        assert_eq!(acp[1]["headers"][0]["value"], "Bearer t");
        assert_eq!(acp.as_array().unwrap().len(), 2);
        assert!(super::claude(&[]).is_none() && super::codex(&[]).is_none());
    }

    #[test]
    fn claude_configs_go_once_their_servers_or_session_do() {
        let dir = std::env::temp_dir().join(format!("mux-mcp-{}", std::process::id()));
        let servers = [stdio("probe")];
        let one = claude_file_in(&dir, &servers, 1).unwrap().unwrap();
        let two = claude_file_in(&dir, &servers, 2).unwrap().unwrap();
        assert!(std::fs::read_to_string(&one).unwrap().contains("probe"));
        assert!(claude_file_in(&dir, &[], 1).unwrap().is_none());
        assert!(!one.exists() && two.exists());
        let three = claude_file_in(&dir, &servers, 3).unwrap().unwrap();
        forget_idle_in(&dir, &[3]);
        assert!(
            !two.exists() && three.exists(),
            "a running claude keeps its file"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn claudes_saved_pictures_are_picked_out_of_the_text() {
        let (text, paths) = image_refs(
            "Rendered\n[Image: source: /home/me/blob.png]\n[Image: source: relative.png]",
        );
        assert_eq!(text, "Rendered\n[Image: source: relative.png]");
        assert_eq!(paths, ["/home/me/blob.png"]);
    }

    #[test]
    fn pictures_in_either_shape_are_saved() {
        let dir = std::env::temp_dir().join(format!("mux-images-{}", std::process::id()));
        // a 1x1 png, small enough to inline
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let content = json!([
            { "type": "text", "text": "here" },
            { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": png } },
            { "type": "image", "data": png, "mimeType": "image/jpeg" },
            { "type": "image", "data": "not base64!" },
        ]);
        let saved = save_images_in(&dir, &content, 4);
        assert_eq!(saved.len(), 2);
        assert!(saved[1].ends_with(".jpg"));
        assert!(std::fs::read(&saved[0]).unwrap().starts_with(b"\x89PNG"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_readable_only_by_their_owner() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("mux-private-{}", std::process::id()));
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        mux_core::write_private(&path, b"secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            (mode, std::fs::read_to_string(&path).unwrap().as_str()),
            (0o600, "secret")
        );
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn forms_become_questions_and_answers_come_back_typed() {
        let schema = json!({ "type": "object", "required": ["color"], "properties": {
            "why": { "type": "string", "title": "Why?" },
            "color": { "type": "string", "title": "Colour", "oneOf": [
                { "const": "r", "title": "Red" }, { "const": "g", "title": "Green" } ] },
            "count": { "type": "integer", "title": "How many" },
            "ok": { "type": "boolean", "title": "Sure?", "description": "Really sure?" },
        } });
        let order = ["color".to_owned(), "why".to_owned()];
        let questions = questions(&schema, &order);
        let ids: Vec<String> = questions.iter().map(|q| q.id.clone()).collect();
        assert_eq!(ids[..2], order);
        assert_eq!(questions[0].options[1].label, "Green");
        assert!(!questions[0].optional && questions[1].optional && questions[1].other);
        let ok = questions.iter().find(|q| q.id == "ok").unwrap();
        assert_eq!(
            (ok.header.as_str(), ok.question.as_str()),
            ("Sure?", "Really sure?")
        );
        let answers: Vec<Vec<String>> = ids
            .iter()
            .map(|id| match id.as_str() {
                "color" => vec!["Green".to_owned()],
                "count" => vec!["3".to_owned()],
                "ok" => vec!["Yes".to_owned()],
                _ => Vec::new(),
            })
            .collect();
        assert_eq!(
            content(&schema, &ids, &answers),
            json!({ "color": "g", "count": 3, "ok": true })
        );
    }
}
