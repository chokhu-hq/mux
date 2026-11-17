use mux_core::protocol::UsageWindow;
use serde_json::Value;

const FIVE_HOURS: u64 = 5 * 60;
const WEEK: u64 = 7 * 24 * 60;
const MONTH: u64 = 30 * 24 * 60;
// claude's stream names its per-model weekly window only by this type, the model's name comes from get_usage
const OVERAGE_INCLUDED: &str = "seven_day_overage_included";

// codex reports a primary and a secondary window, each sized in minutes
pub fn codex(limits: &Value) -> Vec<UsageWindow> {
    // without a length, free and go plans have a monthly allowance where paid plans have five hours
    let monthly = matches!(limits["planType"].as_str(), Some("free" | "go"));
    let primary = if monthly { MONTH } else { FIVE_HOURS };
    let mut windows: Vec<UsageWindow> = [("primary", primary), ("secondary", WEEK)]
        .into_iter()
        .filter_map(|(key, fallback)| {
            let window = &limits[key];
            let minutes = window["windowDurationMins"].as_u64().unwrap_or(fallback);
            Some(UsageWindow {
                label: span(minutes),
                used: (window["usedPercent"].as_f64()? / 100.0) as f32,
                resets_at: window["resetsAt"].as_u64().unwrap_or(0),
                minutes,
                model: None,
            })
        })
        .collect();
    let individual = &limits["individualLimit"];
    if let Some(left) = individual["remainingPercent"].as_f64() {
        windows.push(UsageWindow {
            label: "credit".to_owned(),
            used: ((100.0 - left) / 100.0) as f32,
            resets_at: individual["resetsAt"].as_u64().unwrap_or(0),
            minutes: 0,
            model: None,
        });
    }
    windows
}

// codex's updates leave out what didn't change, so each field sent replaces only its own
pub fn merge_codex(limits: &mut Value, update: &Value) {
    // a model's own limits would otherwise overwrite the plan's
    if update["limitId"].as_str().is_some_and(|id| id != "codex") {
        return;
    }
    let Some(fields) = update.as_object() else {
        return;
    };
    if !limits.is_object() {
        *limits = Value::Object(Default::default());
    }
    for (key, value) in fields {
        limits[key] = value.clone();
    }
}

// claude's stream reports named windows with utilisation already between 0 and 1
pub fn claude(info: &Value) -> Vec<UsageWindow> {
    let Some(windows) = info["unifiedWindows"].as_object() else {
        return Vec::new();
    };
    let mut out: Vec<UsageWindow> = windows
        .iter()
        .map(|(key, window)| {
            let (label, minutes, model) = match key.as_str() {
                "five_hour" => ("5h".to_owned(), FIVE_HOURS, None),
                "seven_day" => ("7d".to_owned(), WEEK, None),
                OVERAGE_INCLUDED => ("7d".to_owned(), WEEK, Some(OVERAGE_INCLUDED.to_owned())),
                other => (other.replace('_', " "), 0, None),
            };
            UsageWindow {
                label,
                used: window["utilization"].as_f64().unwrap_or(0.0) as f32,
                resets_at: window["resetsAt"].as_u64().unwrap_or(0),
                minutes,
                model,
            }
        })
        .collect();
    // shortest window first, matching how codex lists them, and the plan's own before a model's
    out.sort_by_key(|w| (w.label != "5h", w.model.is_some()));
    out
}

// claude's answer to get_usage, with utilisation from 0 to 100 and resets as iso times. none without a plan
pub fn claude_usage(answer: &Value) -> Option<Vec<UsageWindow>> {
    let limits = &answer["rate_limits"];
    if answer["rate_limits_available"] != true || !limits.is_object() {
        return None;
    }
    let window = |w: &Value, label: &str, minutes: u64, model: Option<&str>| {
        Some(UsageWindow {
            label: label.to_owned(),
            used: (w["utilization"].as_f64()? / 100.0) as f32,
            resets_at: w["resets_at"]
                .as_str()
                .and_then(crate::history::timestamp)
                .map_or(0, |at| at.max(0) as u64),
            minutes,
            model: model.map(str::to_owned),
        })
    };
    let mut out: Vec<UsageWindow> = window(&limits["five_hour"], "5h", FIVE_HOURS, None)
        .into_iter()
        .chain(window(&limits["seven_day"], "7d", WEEK, None))
        .collect();
    for scoped in limits["model_scoped"].as_array().into_iter().flatten() {
        if let Some(name) = scoped["display_name"].as_str() {
            out.extend(window(scoped, "7d", WEEK, Some(name)));
        }
    }
    if let Some(used) = spend(limits) {
        out.push(UsageWindow {
            label: "spend".to_owned(),
            used,
            resets_at: 0,
            minutes: 0,
            model: None,
        });
    }
    Some(out)
}

// how much of the monthly cap on paying past the plan is used, when that's turned on
fn spend(limits: &Value) -> Option<f32> {
    let share = |used: &Value, limit: &Value| {
        let (used, limit) = (used.as_f64()?, limit.as_f64()?);
        (used >= 0.0 && limit > 0.0).then(|| (used / limit) as f32)
    };
    let spend = &limits["spend"];
    if spend["enabled"] == true {
        let (used, limit) = (&spend["used"], &spend["limit"]);
        let currency = |v: &Value| v["currency"].as_str().map(|c| c.trim().to_owned());
        // minor units only compare in the same currency and scale
        if currency(used).is_some_and(|c| !c.is_empty())
            && currency(used) == currency(limit)
            && used["exponent"] == limit["exponent"]
            && let Some(share) = share(&used["amount_minor"], &limit["amount_minor"])
        {
            return Some(share);
        }
    }
    let extra = &limits["extra_usage"];
    if extra["is_enabled"] != true {
        return None;
    }
    share(&extra["used_credits"], &extra["monthly_limit"]).or_else(|| {
        let percent = extra["utilization"].as_f64().filter(|u| *u >= 0.0)?;
        Some((percent / 100.0) as f32)
    })
}

// the stream only reports some windows, so they update the ones claude listed before and keep the rest
pub fn merge_claude(known: &[UsageWindow], update: Vec<UsageWindow>) -> Vec<UsageWindow> {
    // claude allows paying past the plan for one model at most, so the stream's model window is the first listed
    let scoped = known.iter().find_map(|w| w.model.clone());
    let mut out = known.to_vec();
    for mut window in update {
        if window.model.as_deref() == Some(OVERAGE_INCLUDED) {
            let Some(name) = scoped.clone() else {
                continue;
            };
            window.model = Some(name);
        }
        match out
            .iter_mut()
            .find(|w| w.label == window.label && w.model == window.model)
        {
            Some(known) => *known = window,
            None => out.push(window),
        }
    }
    out
}

// a window for one model only holds back sessions on that model, and one on a model not known isn't held
fn applies(window: &UsageWindow, model: Option<&str>) -> bool {
    let token = |s: &str| -> String {
        s.chars()
            .filter(char::is_ascii_alphanumeric)
            .map(|c| c.to_ascii_lowercase())
            .collect()
    };
    match &window.model {
        None => true,
        Some(scope) => model.is_some_and(|m| token(m).contains(&token(scope))),
    }
}

// when a claude limit that rejects work resets, unless the account can keep going on paid overage
pub fn claude_blocked_until(info: &Value, model: Option<&str>) -> Option<u64> {
    let overage = matches!(
        info["overageStatus"].as_str(),
        Some("allowed" | "allowed_warning")
    ) || info["isUsingOverage"] == true;
    if info["status"] != "rejected" || overage {
        return None;
    }
    info["resetsAt"]
        .as_u64()
        .or_else(|| {
            let kind = info["rateLimitType"].as_str()?;
            info["unifiedWindows"][kind]["resetsAt"].as_u64()
        })
        .or_else(|| blocked_until(&claude(info), model))
}

// when the last of the used up windows resets, since work waits for all of them
pub fn blocked_until(windows: &[UsageWindow], model: Option<&str>) -> Option<u64> {
    windows
        .iter()
        .filter(|w| w.used >= 1.0 && w.resets_at > 0 && applies(w, model))
        .map(|w| w.resets_at)
        .max()
}

fn span(minutes: u64) -> String {
    if minutes < 24 * 60 {
        format!("{}h", minutes.div_ceil(60))
    } else {
        format!("{}d", minutes / (24 * 60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // shapes taken from what codex 0.164 and claude 2.1.312 sent during the probes
    #[test]
    fn reads_both_agents_limits() {
        let codex = codex(&json!({
            "primary": { "usedPercent": 35, "windowDurationMins": 300, "resetsAt": 10 },
            "secondary": { "usedPercent": 4, "windowDurationMins": 10080, "resetsAt": 20 }
        }));
        assert_eq!(
            codex[0],
            UsageWindow {
                label: "5h".into(),
                used: 0.35,
                resets_at: 10,
                minutes: 300,
                model: None,
            }
        );
        assert_eq!(codex[1].label, "7d");

        let claude = claude(&json!({ "unifiedWindows": {
            "seven_day": { "utilization": 0.29, "resetsAt": 30 },
            "five_hour": { "utilization": 0.02, "resetsAt": 40 }
        } }));
        assert_eq!(claude[0].label, "5h");
        assert!((claude[1].used - 0.29).abs() < 1e-6);
    }

    #[test]
    fn knows_when_a_used_up_limit_lets_work_go_on() {
        let windows = json!({ "five_hour": { "utilization": 1.0, "resetsAt": 50 },
            "seven_day": { "utilization": 0.4, "resetsAt": 90 } });
        let rejected = json!({ "status": "rejected", "rateLimitType": "five_hour", "unifiedWindows": windows });
        assert_eq!(claude_blocked_until(&rejected, None), Some(50));
        let mut overage = rejected.clone();
        overage["overageStatus"] = json!("allowed");
        assert_eq!(
            claude_blocked_until(&overage, None),
            None,
            "paid overage keeps it running"
        );
        let allowed = json!({ "status": "allowed_warning", "unifiedWindows": windows });
        assert_eq!(claude_blocked_until(&allowed, None), None);
        assert_eq!(
            blocked_until(
                &codex(&json!({
                    "primary": { "usedPercent": 100, "windowDurationMins": 300, "resetsAt": 10 },
                    "secondary": { "usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 20 }
                })),
                None
            ),
            Some(20),
            "work waits for every used up window"
        );
    }

    // trimmed from what claude 2.1.325 answers to get_usage, with spend shaped the way t3 code reads it
    #[test]
    fn reads_claudes_usage_answer() {
        let answer = json!({ "rate_limits_available": true, "rate_limits": {
            "five_hour": { "utilization": 12.0, "resets_at": "1970-01-01T01:00:00Z" },
            "seven_day": { "utilization": 40.0, "resets_at": null },
            "model_scoped": [{ "display_name": "Opus", "utilization": 75.0, "resets_at": "1970-01-02T00:00:00+00:00" }],
            "extra_usage": { "is_enabled": true, "monthly_limit": 2000, "used_credits": 500, "utilization": null }
        } });
        let windows = claude_usage(&answer).unwrap();
        let labels: Vec<_> = windows
            .iter()
            .map(|w| (w.label.as_str(), w.model.as_deref()))
            .collect();
        assert_eq!(
            labels,
            [
                ("5h", None),
                ("7d", None),
                ("7d", Some("Opus")),
                ("spend", None)
            ]
        );
        assert_eq!((windows[0].resets_at, windows[0].minutes), (3600, 300));
        assert!((windows[0].used - 0.12).abs() < 1e-6);
        assert_eq!(windows[1].resets_at, 0);
        assert_eq!(windows[2].resets_at, 86_400);
        assert!((windows[3].used - 0.25).abs() < 1e-6);

        let mut spend = answer.clone();
        spend["rate_limits"]["spend"] = json!({ "enabled": true,
            "used": { "amount_minor": 900, "currency": "USD", "exponent": 2 },
            "limit": { "amount_minor": 1000, "currency": "USD", "exponent": 2 } });
        let windows = claude_usage(&spend).unwrap();
        assert!(
            (windows[3].used - 0.9).abs() < 1e-6,
            "spend wins over extra usage"
        );

        spend["rate_limits"]["extra_usage"]["is_enabled"] = json!(false);
        spend["rate_limits"]["spend"]["limit"]["currency"] = json!("EUR");
        assert_eq!(
            claude_usage(&spend).unwrap().len(),
            3,
            "mismatched currencies can't compare"
        );

        assert_eq!(
            claude_usage(&json!({ "rate_limits_available": false, "rate_limits": null })),
            None
        );
    }

    #[test]
    fn a_model_window_only_blocks_its_own_model() {
        let known = claude_usage(&json!({ "rate_limits_available": true, "rate_limits": {
            "model_scoped": [{ "display_name": "Opus", "utilization": 50.0, "resets_at": null }]
        } }))
        .unwrap();
        let streamed = claude(&json!({ "unifiedWindows": {
            "five_hour": { "utilization": 0.3, "resetsAt": 10 },
            "seven_day_overage_included": { "utilization": 1.0, "resetsAt": 70 }
        } }));
        let merged = merge_claude(&known, streamed.clone());
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].model.as_deref(), Some("Opus"));
        assert_eq!((merged[0].used, merged[0].resets_at), (1.0, 70));
        assert_eq!(merged[1].label, "5h");
        assert_eq!(
            merge_claude(&[], streamed).len(),
            1,
            "an unnamed model window is dropped"
        );

        assert_eq!(blocked_until(&merged, Some("claude-opus-4-5")), Some(70));
        assert_eq!(blocked_until(&merged, Some("sonnet")), None);
        assert_eq!(blocked_until(&merged, None), None);
    }

    #[test]
    fn codex_falls_back_on_its_plan_and_merges_partial_updates() {
        let mut limits = json!({});
        merge_codex(
            &mut limits,
            &json!({ "planType": "free",
            "primary": { "usedPercent": 20, "resetsAt": 5 },
            "individualLimit": { "remainingPercent": 70, "resetsAt": 9 } }),
        );
        merge_codex(
            &mut limits,
            &json!({ "limitId": "codex-other", "primary": { "usedPercent": 99 } }),
        );
        merge_codex(&mut limits, &json!({ "secondary": { "usedPercent": 10 } }));
        let windows = codex(&limits);
        let labels: Vec<_> = windows.iter().map(|w| w.label.as_str()).collect();
        assert_eq!(labels, ["30d", "7d", "credit"]);
        assert_eq!((windows[0].used, windows[0].minutes), (0.2, MONTH));
        assert!((windows[2].used - 0.3).abs() < 1e-6);
        assert_eq!(windows[2].resets_at, 9);
        merge_codex(&mut limits, &json!({ "individualLimit": null }));
        assert_eq!(codex(&limits).len(), 2, "a field sent as null is cleared");
    }
}
