use std::io;
use std::process::Stdio;
use std::time::Duration;

use mux_core::protocol::ModelOption;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

// codex only tier that runs faster at a higher cost, which the picker calls fast mode
const FAST_TIER: &str = "priority";

// asks a short lived codex for the models this account can use
pub async fn fetch() -> io::Result<Vec<ModelOption>> {
    tokio::time::timeout(Duration::from_secs(20), list())
        .await
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "codex took too long to list models",
            )
        })?
}

async fn list() -> io::Result<Vec<ModelOption>> {
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
    for msg in [init, json!({ "method": "initialized" }), list] {
        stdin.write_all(format!("{msg}\n").as_bytes()).await?;
    }

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg["id"] != 1 || msg.get("method").is_some() {
            continue;
        }
        if let Some(error) = msg.get("error") {
            return Err(io::Error::other(
                error["message"]
                    .as_str()
                    .unwrap_or("model/list failed")
                    .to_owned(),
            ));
        }
        let models = msg["result"]["data"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        return Ok(models.iter().filter_map(parse).collect());
    }
    Err(io::Error::other("codex exited before listing models"))
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
