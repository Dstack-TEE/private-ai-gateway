//! Printing: compact JSON, or a human rendering chosen by the command.

use std::io::{self, Write};

use desktop_core::client::CallError;
use serde_json::Value;

use super::{
    Action, Agents, App, Models, Profiles, Service, Settings, Token, Usage, WebUi, WebUiPassword,
};
use crate::Global;

/// Prints a command's result as one line of compact JSON or its human rendering.
pub(super) fn print(action: &Action, global: &Global, value: &Value) -> Result<(), CallError> {
    print_raw(render_line(action, global, value)?.as_bytes())
}

pub(super) fn render_line(
    action: &Action,
    global: &Global,
    value: &Value,
) -> Result<String, CallError> {
    let text = if global.json {
        serde_json::to_string(value).map_err(|_| "Cannot encode output")?
    } else {
        render(action, value)
    };
    Ok(format!("{text}\n"))
}

/// Prints `bytes` as they are, in both modes.
pub(super) fn print_raw(bytes: &[u8]) -> Result<(), CallError> {
    write_stdout(bytes)?;
    Ok(())
}

/// Writes to stdout, `Ok(false)` once the reader has gone away, as after
/// `| head`: output then ends quietly.
pub(super) fn write_stdout(bytes: &[u8]) -> Result<bool, String> {
    match io::stdout().lock().write_all(bytes) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(_) => Err("Cannot write output".into()),
    }
}

/// The human rendering of `action`'s result `value`; details by default.
pub(super) fn render(action: &Action, value: &Value) -> String {
    match action {
        Action::Status { .. } | Action::Start { .. } | Action::Stop { offline: false } => {
            status(value)
        }
        Action::Stop { offline: true } => agents(value),
        Action::Service { command } => match command {
            Service::Start => "Backend started.".into(),
            Service::Stop => "Backend stopped.".into(),
            Service::Status => status(value),
        },
        Action::App {
            command: App::Open { .. },
        } => match value["url"].as_str() {
            Some(url) => format!(
                "Web UI (sign in with `pap web-ui password show`):\n{}{}",
                safe(url),
                if value["browserOpened"] == true {
                    "\nOpened in your browser."
                } else {
                    ""
                }
            ),
            None => "Desktop app opened.".into(),
        },
        Action::Profiles { command } => match command {
            Profiles::List => list(
                value,
                &["id", "name", "provider", "remoteUrl"],
                "No profiles.",
            ),
            Profiles::Export { .. } => exported(value),
            Profiles::Use { id } => format!("Selected profile: {}\n{}", safe(id), status(value)),
            Profiles::Login(_) => format!(
                "Account saved. Use private-ai-proxy start to enable protection.\n{}",
                status(value)
            ),
            Profiles::Remove { id } => format!("Deleted profile: {}", safe(id)),
            Profiles::Add { .. } | Profiles::Verify { .. } | Profiles::Edit { .. } => {
                format!("Profile verified and saved.\n{}", status(value))
            }
            Profiles::Show { .. } | Profiles::Import { .. } => details(value),
        },
        Action::Agents { command } => match command {
            Agents::Connect { dry_run: true, .. } | Agents::Disconnect { dry_run: true, .. } => {
                details(value)
            }
            _ => agents(value),
        },
        Action::Models {
            command: Models::List { .. },
        } => list(&value["models"], &["id", "name"], "No models."),
        Action::Usage { command } => match command {
            Usage::List { .. } => {
                let mut lines = usage_list(&value["items"]);
                if let Some(cursor) = value["nextCursor"].as_str() {
                    lines.push_str(&format!("\nNext cursor: {}", safe(cursor)));
                }
                lines
            }
            Usage::Export { .. } => format!("Exported {} usage records.", text(&value["rows"])),
            Usage::Clear => format!("Deleted {} usage records.", text(&value["deleted"])),
            Usage::Show { .. } => details(value),
        },
        Action::Settings { command } => match command {
            Settings::Reset => {
                "Backend settings reset. Profiles, keys and usage kept; protection is off.".into()
            }
            Settings::Set { key, .. } => format!("Updated {}.", safe(&key.key.to_string())),
            Settings::Show | Settings::Schema => details(value),
        },
        Action::Token { command } => match command {
            Token::Show => text(&value["token"]),
            Token::Rotate => "Client key rotated.".into(),
            Token::ClearCredential => "Profile credential removed.".into(),
        },
        Action::WebUi {
            command: WebUi::Password { command },
        } => match command {
            WebUiPassword::Show => text(&value["password"]),
            WebUiPassword::Rotate => "Web UI password rotated.".into(),
        },
        Action::Diagnostics { .. } => exported(value),
        Action::Cli { .. } | Action::Doctor | Action::Completions { .. } => details(value),
    }
}

fn exported(value: &Value) -> String {
    format!("Exported to {}", text(&value["exported"]))
}

/// One line per agent, with its attention note and error indented below.
fn agents(value: &Value) -> String {
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
                "Not detected"
            } else if agent["authorized"] == true {
                "Connected (authorized)"
            } else if agent["connected"] == true {
                "Configured (not authorized)"
            } else if agent["recorded"] == true {
                "Saved connection (inactive)"
            } else {
                "Not connected"
            };
            let mut line = format!("{}  {}  {state}", text(&agent["id"]), text(&agent["name"]));
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

fn status(value: &Value) -> String {
    if value["status"] == "not_running" {
        return "Backend not running.\nStart backend: private-ai-proxy service start\nStart protection: private-ai-proxy start".into();
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
    let backend = &value["backend"];
    if backend.is_object() {
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
    // A labelled line for a boolean field, when the state has it.
    let flag = |label: &str, value: &Value, yes: &str, no: &str| {
        value
            .as_bool()
            .map(|set| format!("{label}: {}", if set { yes } else { no }))
    };
    let network = "Loopback only";
    lines.extend(flag(
        "Service credential",
        &state["apiKeySaved"],
        "Saved (credentials.toml)",
        "Not saved",
    ));
    if state.get("localApi").is_some() {
        lines.push(format!(
            "Local API: {}",
            state["proxyUrl"]
                .as_str()
                .map(safe)
                .unwrap_or_else(|| "Not listening".into())
        ));
        let allowed = &state["localApi"]["allowNetworkAccess"];
        lines.extend(flag("Network access", allowed, "Allowed", network));
    } else if let Some(endpoint) = state["proxyUrl"].as_str() {
        lines.push(format!("Local API: {}", safe(endpoint)));
    }
    let web_ui = &state["webUi"];
    if web_ui["enabled"] == true {
        lines.push(format!(
            "Web UI: {}",
            match (web_ui["url"].as_str(), web_ui["error"].as_str()) {
                (Some(url), _) => safe(url),
                (None, Some(error)) => format!("Not listening ({})", safe(error)),
                (None, None) => "Not listening".into(),
            }
        ));
        let allowed = &web_ui["allowNetworkAccess"];
        lines.extend(flag("Web UI network access", allowed, "Allowed", network));
    }
    if let Some(error) = state["configFiles"]["error"].as_str() {
        lines.push(format!(
            "Settings file not applied (previous settings in effect): {}",
            safe(error)
        ));
    }
    for warning in state["configFiles"]["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        lines.push(format!("Settings warning: {}", safe(warning)));
    }
    lines.extend(flag(
        "Production OS",
        &state["config"]["requireProductionOs"],
        "Required",
        "Development images allowed",
    ));
    let identity = &state["identity"];
    if identity.is_object() {
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
        let usage = &state["sessionUsage"];
        if usage.is_object() {
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
