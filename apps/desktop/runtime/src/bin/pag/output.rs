use super::{Action, Agents, App, Models, Profiles, Service, Settings, Token, Usage};
use serde_json::Value;

pub(super) fn render(action: &Action, value: &Value) -> String {
    match action {
        Action::Status { .. }
        | Action::Start { .. }
        | Action::Stop
        | Action::Service {
            command: Service::Status,
        } => status(value),
        Action::Service {
            command: Service::Start,
        } => "Backend started.".into(),
        Action::Service {
            command: Service::Stop,
        } => "Backend stopped.".into(),
        Action::App { command: App::Open } => "Desktop app opened.".into(),
        Action::Profiles {
            command: Profiles::List,
        } => list(
            value,
            &["id", "name", "provider", "remoteUrl"],
            "No profiles.",
        ),
        Action::Profiles {
            command: Profiles::Export { .. },
        }
        | Action::Diagnostics { .. } => format!("Exported to {}", text(&value["exported"])),
        Action::Profiles {
            command: Profiles::Use { id },
        } => format!("Selected profile: {}\n{}", safe(id), status(value)),
        Action::Profiles {
            command: Profiles::Remove { id },
        } => format!("Deleted profile: {}", safe(id)),
        Action::Profiles {
            command: Profiles::Add { .. } | Profiles::Verify { .. } | Profiles::Edit { .. },
        } => format!("Profile verified and saved.\n{}", status(value)),
        Action::Agents {
            command:
                Agents::List
                | Agents::DisconnectAll
                | Agents::Connect { dry_run: false, .. }
                | Agents::Disconnect { dry_run: false, .. },
        } => {
            let agents = value
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_else(|| std::slice::from_ref(value));
            if agents.is_empty() {
                return "No agents.".into();
            }
            agents
                .iter()
                .map(|agent| {
                    let state = if agent["installed"] == false {
                        "Not installed"
                    } else if agent["authorized"] == true {
                        "Connected (authorized)"
                    } else if agent["connected"] == true {
                        "Configured (not authorized)"
                    } else if agent["recorded"] == true {
                        "Saved connection (inactive)"
                    } else {
                        "Not connected"
                    };
                    let mut line =
                        format!("{}  {}  {state}", text(&agent["id"]), text(&agent["name"]));
                    if let Some(attention) = agent["attention"].as_str().filter(|s| !s.is_empty()) {
                        line.push_str(&format!("\n  {}", safe(attention)));
                    }
                    if let Some(error) = agent["error"].as_str().filter(|s| !s.is_empty()) {
                        line.push_str(&format!("\n  Error: {}", safe(error)));
                    }
                    line
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Action::Models {
            command: Models::List { .. },
        } => list(&value["models"], &["id", "name"], "No models."),
        Action::Usage {
            command: Usage::List { .. },
        } => {
            let mut lines = usage_list(&value["items"]);
            if let Some(cursor) = value["nextCursor"].as_str() {
                lines.push_str(&format!("\nNext cursor: {}", safe(cursor)));
            }
            lines
        }
        Action::Usage {
            command: Usage::Export { .. },
        } => format!("Exported {} usage records.", text(&value["rows"])),
        Action::Usage {
            command: Usage::Clear,
        } => format!("Deleted {} usage records.", text(&value["deleted"])),
        Action::Settings {
            command: Settings::Set { key, .. },
        } => format!("Updated {}.", safe(&key.to_string())),
        Action::Token {
            command: Token::Show,
        } => text(&value["token"]),
        Action::Token {
            command: Token::Rotate,
        } => "Client key rotated.".into(),
        Action::Token {
            command: Token::ClearCredential,
        } => "Profile credential removed.".into(),
        _ => details(value),
    }
}

fn status(value: &Value) -> String {
    if value["status"] == "not_running" {
        return "Backend not running.\nStart backend: pag service start\nStart protection: pag start".into();
    }
    let state = value.get("gateway").unwrap_or(value);
    let status = match state["status"].as_str() {
        _ if state["backendConnected"] == false => "Backend disconnected (protection unknown)",
        _ if state["configurationVerification"] == true => {
            "Verifying profile (not protecting traffic)"
        }
        _ if state["reconnecting"] == true => "Reconnecting (requests paused)",
        Some("verified") => "Protected",
        Some("verifying") => "Verifying",
        Some("blocked") => "Not protected (blocked)",
        Some("error") => "Not protected (error)",
        _ => "Not protected",
    };
    let mut lines = vec![status.to_string()];
    if let Some(backend) = value.get("backend").filter(|backend| backend.is_object()) {
        lines.push(format!(
            "Backend: running (PID {}, version {})",
            text(&backend["processId"]),
            text(&backend["version"])
        ));
    }
    let profile = state["profiles"].as_array().and_then(|profiles| {
        profiles
            .iter()
            .find(|profile| profile["id"] == state["activeProfileId"])
    });
    if let Some(profile) = profile {
        lines.push(format!(
            "Profile: {} ({})",
            text(&profile["name"]),
            text(&profile["id"])
        ));
        lines.push(format!(
            "Service: {} | {}",
            text(&profile["provider"]),
            text(&profile["remoteUrl"])
        ));
    } else if let Some(id) = state["activeProfileId"]
        .as_str()
        .filter(|id| !id.is_empty())
    {
        lines.push(format!("Profile: {}", safe(id)));
    } else if state["profiles"].is_array() {
        lines.push("Profile: None selected".into());
    }
    if let Some(saved) = state["apiKeySaved"].as_bool() {
        lines.push(format!(
            "Service credential: {}",
            if saved {
                "Saved (OS credential store)"
            } else {
                "Not saved"
            }
        ));
    }
    if state.get("localApi").is_some() {
        lines.push(format!(
            "Local API: {}",
            state["proxyUrl"]
                .as_str()
                .map(safe)
                .unwrap_or_else(|| "Not listening".into())
        ));
        if let Some(network) = state["localApi"]["allowNetworkAccess"].as_bool() {
            lines.push(format!(
                "Network access: {}",
                if network { "Allowed" } else { "Loopback only" }
            ));
        }
    } else if let Some(endpoint) = state["proxyUrl"].as_str() {
        lines.push(format!("Local API: {}", safe(endpoint)));
    }
    if let Some(required) = state["config"]["requireProductionOs"].as_bool() {
        lines.push(format!(
            "Production OS: {}",
            if required {
                "Required"
            } else {
                "Development images allowed"
            }
        ));
    }
    if let Some(identity) = state
        .get("identity")
        .filter(|identity| identity.is_object())
    {
        lines.push(format!(
            "Identity: {} | {}",
            text(&identity["teeType"]),
            text(&identity["trustLevel"])
        ));
    }
    if let Some(checks) = state["checks"]
        .as_array()
        .filter(|checks| !checks.is_empty())
    {
        let count = |status: &str| {
            checks
                .iter()
                .filter(|check| check["status"] == status)
                .count()
        };
        lines.push(format!(
            "Checks: {} passed, {} failed, {} skipped",
            count("pass"),
            count("fail"),
            count("skip")
        ));
        for check in checks
            .iter()
            .filter(|check| check["status"] == "fail")
            .take(3)
        {
            lines.push(format!("  Failed: {}", text(&check["title"])));
        }
    }
    if let Some(models) = state["catalog"]["models"].as_array() {
        let label = if state["status"] == "verified"
            && state["configurationVerification"] != true
            && state["backendConnected"] != false
        {
            "Models"
        } else {
            "Cached models"
        };
        lines.push(format!("{label}: {}", models.len()));
    } else if state.get("config").is_some() {
        lines.push("Models: No verified catalog".into());
    }
    if let Some(session) = state["sessionId"].as_str() {
        lines.push(format!("Session: {}", safe(session)));
        if let Some(usage) = state.get("sessionUsage").filter(|usage| usage.is_object()) {
            lines.push(format!(
                "Requests: {} total | {} protected | {} blocked locally | {} failed proof",
                text(&usage["requests"]),
                text(&usage["protected"]),
                text(&usage["blockedLocally"]),
                text(&usage["failedProof"])
            ));
            lines.push(format!(
                "Tokens: {} input | {} output | {} cache read | {} cache write",
                text(&usage["inputTokens"]),
                text(&usage["outputTokens"]),
                text(&usage["cacheReadTokens"]),
                text(&usage["cacheWriteTokens"])
            ));
            if let Some(cost) = usage["costUsd"].as_f64() {
                lines.push(format!("Reported cost: ${cost:.6} USD"));
            }
        }
    }
    for (key, label) in [
        ("progress", "Progress"),
        ("error", "Error"),
        ("endpointError", "Warning"),
    ] {
        if let Some(value) = state[key].as_str().filter(|s| !s.is_empty()) {
            lines.push(format!("{label}: {}", safe(value)));
        }
    }
    if state["wakeMonitorAvailable"] == false {
        lines
            .push("Warning: Wake monitoring unavailable; reconnect protection after sleep.".into());
    }
    lines.join("\n")
}

fn usage_list(value: &Value) -> String {
    let Some(rows) = value.as_array().filter(|rows| !rows.is_empty()) else {
        return "No usage records.".into();
    };
    let mut lines =
        vec!["Id  |  Model  |  HTTP  |  Verification  |  Input tokens  |  Output tokens".into()];
    lines.extend(rows.iter().map(|row| {
        let verdict = if row["leftDevice"] == false {
            "Blocked locally"
        } else if row["verified"] == true {
            "Verified"
        } else if row["verified"] == false {
            "Failed"
        } else {
            "Unverified"
        };
        format!(
            "{}  |  {}  |  {}  |  {verdict}  |  {}  |  {}",
            text(&row["id"]),
            text(&row["model"]),
            text(&row["status"]),
            text(&row["inputTokens"]),
            text(&row["outputTokens"])
        )
    }));
    lines.join("\n")
}

fn list(value: &Value, columns: &[&str], empty: &str) -> String {
    let Some(rows) = value.as_array().filter(|rows| !rows.is_empty()) else {
        return empty.into();
    };
    let mut lines = vec![columns
        .iter()
        .map(|key| label(key))
        .collect::<Vec<_>>()
        .join("  |  ")];
    lines.extend(rows.iter().map(|row| {
        columns
            .iter()
            .map(|key| text(&row[*key]))
            .collect::<Vec<_>>()
            .join("  |  ")
    }));
    lines.join("\n")
}

// Keep uncommon diagnostic and nested settings fields readable without losing them.
pub(super) fn details(value: &Value) -> String {
    match value {
        Value::Object(fields) => fields
            .iter()
            .filter(|(_, value)| !value.is_null())
            .map(|(key, value)| {
                if value.is_object() || value.is_array() {
                    format!(
                        "{}:\n{}",
                        label(key),
                        details(value)
                            .lines()
                            .map(|line| format!("  {line}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )
                } else {
                    format!("{}: {}", label(key), text(value))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Array(items) if items.is_empty() => "None".into(),
        Value::Array(items) => items.iter().map(details).collect::<Vec<_>>().join("\n\n"),
        _ => text(value),
    }
}

fn text(value: &Value) -> String {
    match value {
        Value::Null => "-".into(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::String(value) => safe(value),
        _ => value.to_string(),
    }
}

pub(super) fn safe(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

fn label(key: &str) -> String {
    let mut label = String::new();
    for (index, c) in safe(key).chars().enumerate() {
        if index == 0 {
            label.extend(c.to_uppercase());
        } else if c.is_uppercase() {
            label.push(' ');
            label.extend(c.to_lowercase());
        } else {
            label.push(c);
        }
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn status_includes_operational_context_without_credentials_or_activity() {
        assert!(status(&json!({"status":"not_running"})).contains("pag service start"));
        let value = json!({
            "backend": {"processId":42,"version":"0.1.0"},
            "gateway": {
                "status":"verified", "activeProfileId":"work", "apiKeySaved":true,
                "profiles":[{"id":"work","name":"Work","provider":"redpill","remoteUrl":"https://tee.redpill.ai"}],
                "proxyUrl":"http://127.0.0.1:4180", "localApi":{"allowNetworkAccess":false},
                "config":{"requireProductionOs":true},
                "identity":{"teeType":"tdx","trustLevel":"production"},
                "checks":[{"title":"Identity","status":"pass"}],
                "catalog":{"models":[{"id":"model"}]}, "sessionId":"session-1",
                "sessionUsage":{"requests":12,"protected":10,"blockedLocally":1,"failedProof":1,"inputTokens":100,"outputTokens":20,"cacheReadTokens":5,"cacheWriteTokens":0,"costUsd":0.0012},
                "activity":[{"detail":"private request detail"}], "token":"never-print-token"
            }
        });
        let output = status(&value);
        for expected in [
            "Backend: running (PID 42, version 0.1.0)",
            "Profile: Work (work)",
            "Service: redpill | https://tee.redpill.ai",
            "Production OS: Required",
            "Identity: tdx | production",
            "Checks: 1 passed, 0 failed, 0 skipped",
            "Models: 1",
            "Requests: 12 total | 10 protected | 1 blocked locally | 1 failed proof",
            "Tokens: 100 input | 20 output | 5 cache read | 0 cache write",
            "Reported cost: $0.001200 USD",
        ] {
            assert!(output.contains(expected), "missing {expected}: {output}");
        }
        assert!(!output.contains("private request detail"));
        assert!(!output.contains("never-print-token"));
        let mut state = value["gateway"].clone();
        state["status"] = json!("stopped");
        assert!(status(&state).contains("Cached models: 1"));
        state["status"] = json!("verified");
        state["backendConnected"] = json!(false);
        assert!(status(&state).starts_with("Backend disconnected (protection unknown)"));
        state["backendConnected"] = json!(true);
        state["configurationVerification"] = json!(true);
        assert!(status(&state).starts_with("Verifying profile (not protecting traffic)"));
    }

    #[test]
    fn status_and_nested_details_do_not_emit_terminal_controls() {
        let output = status(
            &json!({"status":"error", "error":"bad\u{001b}[31m\nline", "progress":"Checking", "wakeMonitorAvailable":false}),
        );
        assert!(output.contains("Error: bad [31m line"));
        assert!(output.contains("Progress: Checking"));
        assert!(output.contains("Wake monitoring unavailable"));
        assert!(
            !details(&json!({"\u{001b}key":"\u{202e}value"})).contains(['\u{001b}', '\u{202e}'])
        );
    }

    #[test]
    fn usage_separates_http_status_from_receipt_verification() {
        let output = usage_list(&json!([
            {"id":"a", "status":200, "leftDevice":true, "verified":false},
            {"id":"b", "status":0, "leftDevice":false, "verified":null},
            {"id":"c", "status":200, "leftDevice":true, "verified":true}
        ]));
        assert!(output.contains("200  |  Failed"));
        assert!(output.contains("0  |  Blocked locally"));
        assert!(output.contains("200  |  Verified"));
    }

    #[test]
    fn agent_list_distinguishes_authorization_from_retained_configuration() {
        let output = render(
            &Action::Agents {
                command: Agents::List,
            },
            &json!([
                {"id":"active", "installed":true, "connected":true, "authorized":true},
                {"id":"stale", "installed":true, "connected":true, "authorized":false, "error":"changed\u{001b}"},
                {"id":"saved", "installed":true, "connected":false, "recorded":true}
            ]),
        );
        assert!(output.contains("Connected (authorized)"));
        assert!(output.contains("Configured (not authorized)"));
        assert!(output.contains("Saved connection (inactive)"));
        assert!(output.contains("Error: changed "));
    }
}
