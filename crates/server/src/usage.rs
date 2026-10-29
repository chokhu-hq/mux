use mux_core::protocol::UsageWindow;
use serde_json::Value;

// codex reports a primary and a secondary window, each sized in minutes
pub fn codex(limits: &Value) -> Vec<UsageWindow> {
    ["primary", "secondary"]
        .iter()
        .filter_map(|key| {
            let window = &limits[key];
            let minutes = window["windowDurationMins"].as_u64()?;
            Some(UsageWindow {
                label: span(minutes),
                used: (window["usedPercent"].as_f64().unwrap_or(0.0) / 100.0) as f32,
                resets_at: window["resetsAt"].as_u64().unwrap_or(0),
            })
        })
        .collect()
}

// claude reports named windows with utilisation already between 0 and 1
pub fn claude(info: &Value) -> Vec<UsageWindow> {
    let Some(windows) = info["unifiedWindows"].as_object() else {
        return Vec::new();
    };
    let mut out: Vec<UsageWindow> = windows
        .iter()
        .map(|(key, window)| UsageWindow {
            label: match key.as_str() {
                "five_hour" => "5h".to_owned(),
                "seven_day" => "7d".to_owned(),
                other => other.replace('_', " "),
            },
            used: window["utilization"].as_f64().unwrap_or(0.0) as f32,
            resets_at: window["resetsAt"].as_u64().unwrap_or(0),
        })
        .collect();
    // shortest window first, matching how codex lists them
    out.sort_by_key(|w| if w.label == "5h" { 0 } else { 1 });
    out
}

// when a claude limit that rejects work resets, unless the account can keep going on paid overage
pub fn claude_blocked_until(info: &Value) -> Option<u64> {
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
        .or_else(|| blocked_until(&claude(info)))
}

// when the last of the used up windows resets, since work waits for all of them
pub fn blocked_until(windows: &[UsageWindow]) -> Option<u64> {
    windows
        .iter()
        .filter(|w| w.used >= 1.0 && w.resets_at > 0)
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
                resets_at: 10
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
        assert_eq!(claude_blocked_until(&rejected), Some(50));
        let mut overage = rejected.clone();
        overage["overageStatus"] = json!("allowed");
        assert_eq!(
            claude_blocked_until(&overage),
            None,
            "paid overage keeps it running"
        );
        let allowed = json!({ "status": "allowed_warning", "unifiedWindows": windows });
        assert_eq!(claude_blocked_until(&allowed), None);
        assert_eq!(
            blocked_until(&codex(&json!({
                "primary": { "usedPercent": 100, "windowDurationMins": 300, "resetsAt": 10 },
                "secondary": { "usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 20 }
            }))),
            Some(20),
            "work waits for every used up window"
        );
    }
}
