use std::io;
use std::path::{Path, PathBuf};

use mux_core::protocol::{McpServer, McpTransport, Question, QuestionOption};
use serde_json::{Map, Value, json};

// claude's servers go in a file rather than on its command line, where any process could read the secrets
pub fn claude_file(servers: &[McpServer], session: u64) -> io::Result<Option<PathBuf>> {
    claude_file_in(&folder(), servers, session)
}

// removes one session's claude config, or every session's when the servers change, since each is rewritten
// with the new list the next time its claude starts
pub fn forget_claude(session: Option<u64>) {
    forget_claude_in(&folder(), session);
}

fn folder() -> PathBuf {
    mux_core::protocol::data_dir().join("mcp")
}

fn claude_file_in(dir: &Path, servers: &[McpServer], session: u64) -> io::Result<Option<PathBuf>> {
    let Some(config) = claude(servers) else {
        // keys from servers since turned off or removed don't stay behind
        forget_claude_in(dir, Some(session));
        return Ok(None);
    };
    mux_core::private_dir(dir)?;
    let path = dir.join(format!("claude-{session}.json"));
    mux_core::write_private(&path, config.as_bytes())?;
    Ok(Some(path))
}

fn forget_claude_in(dir: &Path, session: Option<u64>) {
    let paths: Vec<PathBuf> = match session {
        Some(session) => vec![dir.join(format!("claude-{session}.json"))],
        None => std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("claude-") && n.ends_with(".json"))
            })
            .collect(),
    };
    for path in paths {
        let _ = std::fs::remove_file(path);
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
        forget_claude_in(&dir, None);
        assert!(!two.exists());
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
