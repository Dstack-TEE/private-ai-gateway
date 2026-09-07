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
        return "Backend not running.".into();
    }
    let state = value.get("gateway").unwrap_or(value);
    let status = match state["status"].as_str() {
        Some("verified") if state["configurationVerification"] != true => "Protected",
        Some("verifying") => "Verifying",
        Some("blocked") => "Not protected (blocked)",
        Some("error") => "Not protected (error)",
        _ => "Not protected",
    };
    let mut lines = vec![status.to_string()];
    for (key, label) in [
        ("activeProfileId", "Profile"),
        ("proxyUrl", "Local API"),
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
