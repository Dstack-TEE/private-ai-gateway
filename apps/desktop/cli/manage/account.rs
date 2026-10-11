//! CLI account authorization uses the same runtime session as the desktop UI.
use super::{args::AccountLoginOptions, open_browser};
use crate::Global;
use desktop_core::{
    client::{CallError, Client},
    contracts::*,
    protocol::rpc,
};
use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

struct Pending<'a> {
    client: &'a Client,
    id: Option<String>,
}
impl Drop for Pending<'_> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            if self.client.call(rpc::CancelAccountLogin { id }).is_err() {
                // `eprintln!` panics when stderr is gone; a destructor must not.
                let _ = writeln!(
                    io::stderr(),
                    "Account cleanup could not complete; unused authorization expires automatically."
                );
            }
        }
    }
}

pub(super) fn login(
    client: &Client,
    global: &Global,
    options: &AccountLoginOptions,
) -> Result<AppStateWire, CallError> {
    Client::ensure_service()?;
    let state = client.state()?;
    let existing = state
        .profiles
        .iter()
        .find(|profile| profile.id == options.id);
    let provider = options
        .provider
        .map(Into::into)
        .or_else(|| existing.map(|p| p.provider))
        .unwrap_or(ServiceProvider::Redpill);
    if provider == ServiceProvider::Custom {
        return Err(
            "Account login supports Phala and RedPill. Use profiles add for a custom provider."
                .into(),
        );
    }
    let label = provider.label();
    let remote_url = provider
        .preset_url()
        .ok_or("Account login requires a provider preset")?;
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
    let login: desktop_core::account::LoginPresentation = client.call(rpc::BeginAccountLogin {
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
    let deadline = Instant::now() + Duration::from_secs(options.timeout);
    let details: AccountLoginDetails = loop {
        if Instant::now() >= deadline {
            return Err("Account login timed out; retry profiles login.".into());
        }
        if let Some(details) = client.call(rpc::PollAccountLogin {
            id: login.id.clone(),
        })? {
            break details;
        }
        std::thread::sleep(Duration::from_millis(500));
    };
    let workspace_id = workspace(&details, options.workspace, global)?;
    let operation_id = uuid::Uuid::new_v4().to_string();
    let initial = client.call(rpc::BeginAccountSave {
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
        Err(_) => client.call(rpc::AccountSaveResult {
            operation_id: operation_id.clone(),
        })?,
    };

    loop {
        match result {
            AccountSaveResult::Complete { state } => return Ok(*state),
            AccountSaveResult::Failed { error } => return Err(error.into()),
            AccountSaveResult::Running => {
                std::thread::sleep(Duration::from_millis(500));
                result = client.call(rpc::AccountSaveResult {
                    operation_id: operation_id.clone(),
                })?;
            }
        }
    }
}

/// The account workspace to save: the requested one, the only one, or one
/// the user picks from the listed workspaces.
fn workspace(
    details: &AccountLoginDetails,
    requested: Option<i64>,
    global: &Global,
) -> Result<Option<i64>, String> {
    let listed = |id| {
        details
            .workspaces
            .iter()
            .any(|workspace| workspace.id == id)
    };
    match (requested, details.workspaces.as_slice()) {
        (_, []) => return Ok(None),
        (Some(id), _) if !listed(id) => {
            return Err("The requested workspace is not available to this account.".into())
        }
        (Some(id), _) => return Ok(Some(id)),
        (None, [only]) => return Ok(Some(only.id)),
        (None, workspaces) => {
            for workspace in workspaces {
                eprintln!("{}: {}", workspace.id, workspace.name.escape_default());
            }
        }
    }
    if !global.interactive() {
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
    if listed(id) {
        Ok(Some(id))
    } else {
        Err("Choose a listed workspace.".into())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    #[test]
    fn oauth_login_supports_headless_device_code() {
        let parsed = crate::Cli::try_parse_from([
            "private-ai-proxy",
            "--yes",
            "profiles",
            "login",
            "work",
            "--provider",
            "redpill",
            "--workspace",
            "123",
            "--no-browser",
        ])
        .unwrap();
        let crate::Command::Manage(super::super::Action::Profiles {
            command: super::super::Profiles::Login(options),
        }) = parsed.command
        else {
            panic!("Expected login")
        };
        assert_eq!(options.id, "work");
        assert_eq!(options.workspace, Some(123));
        assert!(options.no_browser);
        assert!(crate::Cli::try_parse_from([
            "private-ai-proxy",
            "profiles",
            "login",
            "work",
            "--callback-stdin",
        ])
        .is_err());
    }
}
