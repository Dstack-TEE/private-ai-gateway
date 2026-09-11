//! CLI account authorization uses the same runtime session as the desktop UI.
use super::{args::AccountLoginOptions, service_provider, value, Cli};
use crate::{client::Client, contracts::*, protocol::Command};
use serde_json::Value;
use std::{
    io::{self, IsTerminal, Read, Write},
    time::{Duration, Instant},
};

struct Pending<'a> {
    client: &'a Client,
    id: Option<String>,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            if self
                .client
                .request::<Value>(Command::CancelAccountLogin { id })
                .is_err()
            {
                eprintln!("Account cleanup could not complete; unused authorization expires automatically.");
            }
        }
    }
}

pub(super) fn login(
    client: &Client,
    cli: &Cli,
    options: &AccountLoginOptions,
) -> Result<Value, String> {
    Client::ensure_service()?;
    let state = client.state()?;
    let existing = state
        .profiles
        .iter()
        .find(|profile| profile.id == options.id);
    let provider = options
        .provider
        .map(service_provider)
        .or_else(|| existing.map(|p| p.provider.clone()))
        .unwrap_or(ServiceProvider::Redpill);
    if provider == ServiceProvider::Custom {
        return Err(
            "Account login supports Phala and RedPill. Use profiles add for a custom provider."
                .into(),
        );
    }
    if options.callback_stdin && provider != ServiceProvider::Redpill {
        return Err("Phala uses a browser device code; omit --callback-stdin.".into());
    }
    let (label, remote_url) = match provider {
        ServiceProvider::Phala => ("Phala", "https://inference.phala.com"),
        _ => ("RedPill", "https://tee.redpill.ai"),
    };
    let profile = ConfidentialProfileInput {
        id: options.id.clone(),
        name: options
            .name
            .clone()
            .or_else(|| existing.map(|p| p.name.clone()))
            .unwrap_or_else(|| label.into()),
        provider,
        remote_url: remote_url.into(),
    };
    let login: crate::account_login::LoginPresentation =
        client.request(Command::BeginAccountLogin {
            profile: profile.clone(),
        })?;
    let mut pending = Pending {
        client,
        id: Some(login.id.clone()),
    };
    eprintln!("Open this URL to sign in:\n{}", login.url);
    if let Some(code) = &login.user_code {
        eprintln!("Confirm device code: {code}");
    }
    if !options.no_browser && open_browser(&login.url).is_err() {
        eprintln!("Browser did not open. Open the URL above manually.");
    }
    if options.callback_stdin {
        let callback = read_callback()?;
        client.request::<Value>(Command::CompleteAccountLogin {
            id: login.id.clone(),
            callback_url: callback,
        })?;
    }
    let deadline = Instant::now() + Duration::from_secs(options.timeout);
    let details: AccountLoginDetails = loop {
        if Instant::now() >= deadline {
            return Err("Account login timed out; retry profiles login.".into());
        }
        if let Some(details) =
            client.request::<Option<AccountLoginDetails>>(Command::PollAccountLogin {
                id: login.id.clone(),
            })?
        {
            break details;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let workspace_id = if details.workspaces.is_empty() {
        None
    } else if let Some(id) = options.workspace {
        if !details
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
        {
            return Err("The requested workspace is not available to this account.".into());
        }
        Some(id)
    } else if details.workspaces.len() == 1 {
        Some(details.workspaces[0].id)
    } else {
        for workspace in &details.workspaces {
            eprintln!("{}: {}", workspace.id, workspace.name.escape_default());
        }
        if cli.non_interactive || cli.json || !io::stdin().is_terminal() {
            return Err("Choose a workspace with --workspace <id> and retry login.".into());
        }
        eprint!("Workspace ID: ");
        io::stderr()
            .flush()
            .map_err(|_| "Cannot show workspace prompt")?;
        let mut input = String::new();
        io::stdin()
            .read_line(&mut input)
            .map_err(|_| "Cannot read workspace selection")?;
        let id = input
            .trim()
            .parse::<i64>()
            .map_err(|_| "Invalid workspace ID")?;
        if !details
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
        {
            return Err("Choose a listed workspace.".into());
        }
        Some(id)
    };
    let operation_id = uuid::Uuid::new_v4().to_string();
    let initial = client.request::<AccountSaveResult>(Command::SaveAccountLogin {
        operation_id: operation_id.clone(),
        id: login.id,
        profile,
        require_production_os: state.config.require_production_os,
        workspace_id,
    });
    // Once submitted, an uncertain response must be reconciled by operation ID.
    pending.id = None;
    let mut result = match initial {
        Ok(result) => result,
        Err(_) => client.request(Command::AccountSaveResult {
            operation_id: operation_id.clone(),
        })?,
    };

    loop {
        match result {
            AccountSaveResult::Complete { state } => return value(state),
            AccountSaveResult::Failed { error } => return Err(error),
            AccountSaveResult::Running => {
                std::thread::sleep(Duration::from_millis(500));
                result = client.request(Command::AccountSaveResult {
                    operation_id: operation_id.clone(),
                })?;
            }
        }
    }
}

fn read_callback() -> Result<String, String> {
    if io::stdin().is_terminal() {
        return rpassword::prompt_password("Paste callback URL: ")
            .map_err(|_| "Cannot read callback URL".into());
    }
    let mut bytes = Vec::new();
    io::stdin()
        .take(16385)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read callback URL")?;
    if bytes.len() > 16384 {
        return Err("Callback URL is too long".into());
    }
    String::from_utf8(bytes).map_err(|_| "Callback URL must be UTF-8".into())
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    #[cfg(target_os = "windows")]
    let result = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    let mut child = result.map_err(|_| "Cannot open browser")?;
    // Reap the short-lived OS launcher independently of authorization polling.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn oauth_login_supports_headless_callback_without_a_secret_argument() {
        let parsed = super::super::Cli::try_parse_from([
            "pap",
            "--yes",
            "profiles",
            "login",
            "work",
            "--provider",
            "redpill",
            "--workspace",
            "123",
            "--no-browser",
            "--callback-stdin",
        ])
        .unwrap();
        let super::super::Action::Profiles {
            command: super::super::Profiles::Login(options),
        } = parsed.command
        else {
            panic!("Expected login")
        };
        assert_eq!(options.id, "work");
        assert_eq!(options.workspace, Some(123));
        assert!(options.no_browser && options.callback_stdin);
        assert!(super::super::Cli::try_parse_from([
            "pap",
            "profiles",
            "login",
            "work",
            "--callback-url",
            "secret"
        ])
        .is_err());
    }
}
