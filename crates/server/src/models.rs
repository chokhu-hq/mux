use std::io;
use std::process::Stdio;
use std::time::Duration;

use mux_core::Provider;
use mux_core::protocol::{ModelOption, UsageWindow};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::usage;

// codex only tier that runs faster at a higher cost, which the picker calls fast mode
const FAST_TIER: &str = "priority";

pub struct Catalog {
    pub models: Vec<ModelOption>,
    // codex can report its limits up front, claude only reports them during a turn
    pub usage: Option<Vec<UsageWindow>>,
}

// asks a short lived copy of the agent cli for the models this account can use
pub async fn fetch(provider: Provider) -> io::Result<Catalog> {
    let list = async {
        match provider {
            Provider::Codex => list().await,
            Provider::Claude => list_claude().await,
            Provider::OpenCode => list_opencode().await,
        }
    };
    tokio::time::timeout(Duration::from_secs(30), list)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "took too long to list models"))?
}

async fn list() -> io::Result<Catalog> {
    let mut child = Command::new("codex")
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;

    let init = json!({ "id": 0, "method": "initialize", "params": { "clientInfo": { "name": "mux", "version": mux_core::VERSION } } });
    let list = json!({ "id": 1, "method": "model/list", "params": {} });
    let limits = json!({ "id": 2, "method": "account/rateLimits/read", "params": {} });
    for msg in [init, json!({ "method": "initialized" }), list, limits] {
        stdin.write_all(format!("{msg}\n").as_bytes()).await?;
    }

    let mut models: Option<Vec<ModelOption>> = None;
    let mut usage: Option<Vec<UsageWindow>> = None;
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg.get("method").is_some() {
            continue;
        }
        match msg["id"].as_u64() {
            Some(1) => {
                if let Some(error) = msg.get("error") {
                    return Err(io::Error::other(
                        error["message"]
                            .as_str()
                            .unwrap_or("model/list failed")
                            .to_owned(),
                    ));
                }
                let data = msg["result"]["data"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                models = Some(data.iter().filter_map(parse).collect());
            }
            // limits are a bonus, so a failure here just leaves them unknown
            Some(2) => usage = Some(usage::codex(&msg["result"]["rateLimits"])),
            _ => {}
        }
        if let (Some(models), Some(usage)) = (&models, &usage) {
            return Ok(Catalog {
                models: models.clone(),
                usage: Some(usage.clone()).filter(|u| !u.is_empty()),
            });
        }
    }
    Err(io::Error::other("codex exited before listing models"))
}

// opencode's background service lists every model it can reach, with the efforts each one takes
async fn list_opencode() -> io::Result<Catalog> {
    let output = Command::new("opencode")
        .args(["api", "GET", "/api/model"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await?;
    if !output.status.success() {
        return Err(io::Error::other("opencode couldn't list its models"));
    }
    let list: Value = serde_json::from_slice(&output.stdout).map_err(io::Error::other)?;
    Ok(Catalog {
        models: parse_opencode(&list),
        usage: None,
    })
}

fn parse_opencode(list: &Value) -> Vec<ModelOption> {
    let mut models: Vec<ModelOption> = list["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["enabled"] != false && m["status"] != "deprecated")
        .filter_map(|m| {
            let provider = m["providerID"].as_str()?;
            let id = m["id"].as_str()?;
            let efforts: Vec<String> = m["variants"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v["id"].as_str().map(str::to_owned))
                .collect();
            Some(ModelOption {
                // opencode names a model by its provider and id together
                id: format!("{provider}/{id}"),
                name: m["name"].as_str().unwrap_or(id).to_owned(),
                description: provider.to_owned(),
                default_effort: efforts
                    .iter()
                    .find(|e| *e == "medium")
                    .or(efforts.first())
                    .cloned()
                    .unwrap_or_default(),
                efforts,
                is_default: false,
                fast: None,
            })
        })
        .collect();
    // opencode lists the model it starts sessions on first
    if let Some(first) = models.first_mut() {
        first.is_default = true;
    }
    models
}

// claude reports its models in the reply to the initialize control request, before any prompt is sent
async fn list_claude() -> io::Result<Catalog> {
    let mut child = Command::new("claude")
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;
    let init = json!({ "type": "control_request", "request_id": "mux-models", "request": { "subtype": "initialize" } });
    stdin.write_all(format!("{init}\n").as_bytes()).await?;

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg["type"] != "control_response" {
            continue;
        }
        let response = &msg["response"];
        if response["subtype"] == "error" {
            return Err(io::Error::other(
                response["error"]
                    .as_str()
                    .unwrap_or("initialize failed")
                    .to_owned(),
            ));
        }
        let models = response["response"]["models"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        return Ok(Catalog {
            models: models.iter().filter_map(parse_claude).collect(),
            usage: None,
        });
    }
    Err(io::Error::other("claude exited before listing models"))
}

fn parse_claude(model: &Value) -> Option<ModelOption> {
    Some(ModelOption {
        id: model["value"].as_str()?.to_owned(),
        name: model["displayName"].as_str().unwrap_or_default().to_owned(),
        description: model["description"].as_str().unwrap_or_default().to_owned(),
        efforts: model["supportedEffortLevels"]
            .as_array()
            .map(|e| {
                e.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        // claude picks its own effort when none is set, so there is no level to show as the default
        default_effort: String::new(),
        is_default: model["value"] == "default",
        fast: (model["supportsFastMode"] == true)
            .then(|| "Faster output, uses more of your plan".to_owned()),
    })
}

fn parse(model: &Value) -> Option<ModelOption> {
    if model["hidden"] == true {
        return None;
    }
    let fast = model["serviceTiers"].as_array().and_then(|tiers| {
        tiers.iter().find(|t| t["id"] == FAST_TIER).map(|t| {
            t["description"]
                .as_str()
                .unwrap_or("Faster responses")
                .to_owned()
        })
    });
    Some(ModelOption {
        id: model["id"].as_str()?.to_owned(),
        name: model["displayName"].as_str().unwrap_or_default().to_owned(),
        description: model["description"].as_str().unwrap_or_default().to_owned(),
        efforts: model["supportedReasoningEfforts"]
            .as_array()
            .map(|efforts| {
                efforts
                    .iter()
                    .filter_map(|e| e["reasoningEffort"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        default_effort: model["defaultReasoningEffort"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        is_default: model["isDefault"] == true,
        fast,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // trimmed from what codex 0.164 returned for model/list
    // trimmed from what claude 2.1.312 returned in its initialize response
    #[test]
    fn parses_a_claude_model() {
        let model = json!({
            "value": "opus", "displayName": "Opus 5.5", "description": "For complex work",
            "supportedEffortLevels": ["low", "high", "max"], "supportsFastMode": true
        });
        let parsed = parse_claude(&model).unwrap();
        assert_eq!(parsed.id, "opus");
        assert_eq!(parsed.efforts, ["low", "high", "max"]);
        assert!(parsed.fast.is_some() && !parsed.is_default);
        assert!(
            parse_claude(&json!({ "value": "default" }))
                .unwrap()
                .is_default
        );
    }

    #[test]
    fn parses_a_model_with_a_fast_tier() {
        let model = json!({
            "id": "gpt-6.1-sol", "displayName": "GPT-6.1-Sol", "description": "Frontier model",
            "hidden": false, "isDefault": true, "defaultReasoningEffort": "low",
            "supportedReasoningEfforts": [
                { "reasoningEffort": "low", "description": "" },
                { "reasoningEffort": "high", "description": "" }
            ],
            "serviceTiers": [{ "id": "priority", "name": "Fast", "description": "2x speed, increased usage" }]
        });
        let parsed = parse(&model).unwrap();
        assert_eq!(parsed.efforts, ["low", "high"]);
        assert_eq!(parsed.fast.as_deref(), Some("2x speed, increased usage"));
        assert!(parsed.is_default);
        assert!(parse(&json!({ "id": "x", "hidden": true })).is_none());
    }
}
