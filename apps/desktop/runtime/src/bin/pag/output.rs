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
        } => format!("Selected profile: {id}"),
        Action::Profiles {
            command: Profiles::Remove { id },
        } => format!("Deleted profile: {id}"),
        Action::Profiles {
            command: Profiles::Add { .. } | Profiles::Verify { .. },
        } => status(value),
        Action::Agents {
            command: Agents::List | Agents::DisconnectAll,
        } => {
            let Some(agents) = value.as_array() else {
                return details(value);
            };
            if agents.is_empty() {
                return "No agents.".into();
            }
            agents
                .iter()
                .map(|agent| {
                    let state = if agent["installed"] == false {
                        "Not installed"
                    } else if agent["connected"] == true {
                        "Connected"
                    } else {
                        "Not connected"
                    };
                    let mut line =
                        format!("{}  {}  {state}", text(&agent["id"]), text(&agent["name"]));
                    if let Some(attention) = agent["attention"].as_str().filter(|s| !s.is_empty()) {
                        line.push_str(&format!("\n  {attention}"));
                    }
                    line
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        Action::Agents {
            command: Agents::Connect {
                id, dry_run: false, ..
            },
        } => format!("Connected {id}. Configuration is applied while protected."),
        Action::Agents {
            command: Agents::Disconnect { id, dry_run: false },
        } => format!("Disconnected {id}."),
        Action::Models {
            command: Models::List { .. },
        } => list(&value["models"], &["id", "name"], "No models."),
        Action::Usage {
            command: Usage::List { .. },
        } => {
            let mut lines = list(
                &value["items"],
                &["id", "model", "status", "inputTokens", "outputTokens"],
                "No usage records.",
            );
            if let Some(cursor) = value["nextCursor"].as_str() {
                lines.push_str(&format!("\nNext cursor: {cursor}"));
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
        } => format!("Updated {key}."),
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
        Some("error") => "Not protected (error)",
        _ => "Not protected",
    };
    let mut lines = vec![status.to_string()];
    for (key, label) in [
        ("activeProfileId", "Profile"),
        ("proxyUrl", "Local API"),
        ("endpointError", "Warning"),
    ] {
        if let Some(value) = state[key].as_str().filter(|s| !s.is_empty()) {
            lines.push(format!("{label}: {value}"));
        }
    }
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
        Value::String(value) => value
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect(),
        _ => value.to_string(),
    }
}

fn label(key: &str) -> String {
    let mut label = String::new();
    for (index, c) in key.chars().enumerate() {
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
