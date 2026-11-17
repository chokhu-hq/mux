use std::io;
use std::process::Stdio;
use std::time::Duration;

use mux_core::Provider;
use mux_core::protocol::{LimitReset, ModelOption, ResetResult, UsageWindow};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

use crate::usage;

// codex only tier that runs faster at a higher cost, which the picker calls fast mode
const FAST_TIER: &str = "priority";

pub struct Catalog {
    pub models: Vec<ModelOption>,
    // the plan limits, which both can report up front
    pub usage: Option<Vec<UsageWindow>>,
    // the limit resets the account has banked, which only codex has
    pub resets: Option<Vec<LimitReset>>,
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

// a short lived codex app-server: the requests go out after its handshake, and each reply goes to `answer` until
// it has what it waited for. only replies count, so a request codex makes of mux is never taken for an answer
pub(crate) async fn codex_rpc<T>(
    requests: Vec<Value>,
    experimental: bool,
    exited: &str,
    mut answer: impl FnMut(&Value) -> Option<io::Result<T>>,
) -> io::Result<T> {
    let mut child = Command::from(mux_core::command("codex"))
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
    let mut params = json!({ "clientInfo": { "name": "mux", "version": mux_core::VERSION } });
    if experimental {
        params["capabilities"] = json!({ "experimentalApi": true });
    }
    let init = json!({ "id": 0, "method": "initialize", "params": params });
    for msg in [init, json!({ "method": "initialized" })]
        .into_iter()
        .chain(requests)
    {
        stdin.write_all(format!("{msg}\n").as_bytes()).await?;
    }
    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg.get("method").is_some() {
            continue;
        }
        if let Some(result) = answer(&msg) {
            return result;
        }
    }
    Err(io::Error::other(exited.to_owned()))
}

async fn list() -> io::Result<Catalog> {
    let list = json!({ "id": 1, "method": "model/list", "params": {} });
    let limits = json!({ "id": 2, "method": "account/rateLimits/read", "params": {} });
    let mut models: Option<Vec<ModelOption>> = None;
    let mut usage: Option<Vec<UsageWindow>> = None;
    let mut resets = None;
    codex_rpc(
        vec![list, limits],
        false,
        "codex exited before listing models",
        |msg| {
            match msg["id"].as_u64() {
                Some(1) => {
                    if let Some(error) = msg.get("error") {
                        return Some(Err(io::Error::other(
                            error["message"]
                                .as_str()
                                .unwrap_or("model/list failed")
                                .to_owned(),
                        )));
                    }
                    let data = msg["result"]["data"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    models = Some(data.iter().filter_map(parse).collect());
                }
                // limits are a bonus, so a failure here just leaves them unknown
                Some(2) => {
                    let result = &msg["result"];
                    // the plan's own limits, rather than one model's, when codex splits them up
                    let limits = Some(&result["rateLimitsByLimitId"]["codex"])
                        .filter(|l| l.is_object())
                        .unwrap_or(&result["rateLimits"]);
                    usage = Some(usage::codex(limits));
                    resets = Some(parse_resets(&msg["result"]["rateLimitResetCredits"]));
                }
                _ => {}
            }
            let (models, usage) = (models.as_ref()?, usage.as_ref()?);
            Some(Ok(Catalog {
                models: models.clone(),
                usage: Some(usage.clone()).filter(|u| !u.is_empty()),
                resets: resets.clone(),
            }))
        },
    )
    .await
}

// the resets codex says are still there to use, from its rate limit read
fn parse_resets(summary: &Value) -> Vec<LimitReset> {
    let listed: Vec<LimitReset> = summary["credits"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["status"] == "available")
        .map(|c| LimitReset {
            title: c["title"].as_str().unwrap_or("Limit reset").to_owned(),
            expires_at: c["expiresAt"].as_u64(),
        })
        .collect();
    // codex may cap the details it sends, so the count can be more than the rows
    let count = summary["availableCount"].as_u64().unwrap_or(0) as usize;
    let mut resets = listed;
    while resets.len() < count {
        resets.push(LimitReset {
            title: "Limit reset".to_owned(),
            expires_at: None,
        });
    }
    resets
}

// spends one of the account's banked resets through a short lived app-server
pub async fn use_codex_reset() -> ResetResult {
    // the key makes this one attempt, so a retry can never spend a second reset
    let key = format!(
        "mux-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    );
    let consume = json!({ "id": 1, "method": "account/rateLimitResetCredit/consume", "params": { "idempotencyKey": key } });
    let attempt = codex_rpc(
        vec![consume],
        true,
        "codex exited before answering",
        |msg| {
            if msg["id"].as_u64() != Some(1) {
                return None;
            }
            if let Some(error) = msg.get("error") {
                let message = error["message"].as_str().unwrap_or("codex refused");
                return Some(Ok(ResetResult::Failed(message.to_owned())));
            }
            Some(Ok(match msg["result"]["outcome"].as_str() {
                Some("reset") | Some("alreadyRedeemed") => ResetResult::Reset,
                Some("nothingToReset") => ResetResult::NothingToReset,
                Some("noCredit") => ResetResult::NoneLeft,
                other => ResetResult::Failed(format!("codex answered {other:?}")),
            }))
        },
    );
    match tokio::time::timeout(Duration::from_secs(30), attempt).await {
        Ok(Ok(result)) => result,
        Ok(Err(err)) => ResetResult::Failed(err.to_string()),
        Err(_) => ResetResult::Failed("codex took too long to answer".to_owned()),
    }
}

// opencode's background service lists every model it can reach, with the efforts each one takes
async fn list_opencode() -> io::Result<Catalog> {
    let output = Command::from(mux_core::command("opencode"))
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
        resets: None,
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
    let mut command = Command::from(mux_core::command("claude"));
    crate::agent::forget_parent_claude(&mut command);
    let mut child = command
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
    let listed = claude_models(&mut child).await;
    // it may be refreshing the sign-in it started with, which stopping it straight away would lose
    tokio::spawn(crate::agent::stop_claude(child));
    listed
}

async fn claude_models(child: &mut tokio::process::Child) -> io::Result<Catalog> {
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
    let models = loop {
        let Some(line) = lines.next_line().await? else {
            return Err(io::Error::other("claude exited before listing models"));
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if msg["type"] != "control_response" || msg["response"]["request_id"] != "mux-models" {
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
        break models.iter().filter_map(parse_claude).collect();
    };

    // the plan limits are a bonus with their own deadline, so a slow answer never loses the models
    let usage = json!({ "type": "control_request", "request_id": "mux-usage",
        "request": { "subtype": "get_usage", "skip_behaviors": true } });
    let limits = async {
        stdin
            .write_all(format!("{usage}\n").as_bytes())
            .await
            .ok()?;
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if msg["type"] == "control_response" && msg["response"]["request_id"] == "mux-usage" {
                return usage::claude_usage(&msg["response"]["response"]).filter(|u| !u.is_empty());
            }
        }
        None
    };
    let usage = tokio::time::timeout(Duration::from_secs(10), limits)
        .await
        .ok()
        .flatten();
    Ok(Catalog {
        models,
        usage,
        resets: None,
    })
}

fn parse_claude(model: &Value) -> Option<ModelOption> {
    let description = model["description"].as_str().unwrap_or_default();
    let mut name = model["displayName"].as_str().unwrap_or_default().to_owned();
    // claude's default entry is called "Default (recommended)" and says which model it is only in its
    // description, "Opus 5.5 · Best for everyday, complex tasks", so the name carries the model instead
    if model["value"] == "default"
        && let Some((model_name, _)) = description.split_once(" · ")
    {
        name = format!("Default ({model_name})");
    }
    Some(ModelOption {
        id: model["value"].as_str()?.to_owned(),
        name,
        description: description.to_owned(),
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
        let default = json!({
            "value": "default", "displayName": "Default (recommended)",
            "description": "Opus 5.5 · Best for everyday, complex tasks"
        });
        assert_eq!(parse_claude(&default).unwrap().name, "Default (Opus 5.5)");
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
