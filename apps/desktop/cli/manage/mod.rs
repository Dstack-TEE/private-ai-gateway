//! The management commands, which drive the per-user backend through its
//! local API as the desktop app does.

use std::{
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use clap::CommandFactory;
use desktop_core::{
    client::{CallError, Client, Watched},
    contracts::*,
    protocol::{export_path, rpc, NotificationKind, Preference},
    usage::UsageQuery,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Map, Value};

use crate::Global;

mod account;
mod args;
mod install;
mod output;

pub use args::Action;
use args::*;

pub fn run(global: &Global, action: &Action) -> Result<(), CallError> {
    let client = Client::new();
    let value = match action {
        Action::Status { watch: true } => return watch_status(global),
        Action::Status { watch: false }
        | Action::Service {
            command: Service::Status,
        } => status(&client)?,
        Action::Service { command } => service(&client, global, command)?,
        Action::Start { profile, timeout } => start(&client, profile.as_deref(), *timeout)?,
        Action::Stop { offline: false } => value(client.call(rpc::Stop)?)?,
        Action::Stop { offline: true } => restore_agents_offline(&client)?,
        Action::Profiles { command } => profiles(&client, global, command)?,
        Action::Agents { command } => agents(&client, global, command)?,
        Action::Models {
            command: Models::List { refresh },
        } => {
            let state = if *refresh {
                client.call(rpc::RefreshCatalog)?.state
            } else {
                client.state()?
            };
            value(state.catalog.ok_or(
                "No verified model catalog. Start protection and wait for verification first.",
            )?)?
        }
        Action::Usage {
            command: Usage::Show { id, receipt: true },
        } => {
            let receipt = client
                .call(rpc::GetUsageReceipt {
                    record_id: id.clone(),
                })?
                .ok_or("No receipt is saved for this usage record.")?;
            return output::print_raw(receipt.as_bytes());
        }
        Action::Usage { command } => usage(&client, global, command)?,
        Action::Settings {
            command: Settings::Schema,
        } => {
            let schema = desktop_core::config::schema();
            return output::print_raw(format!("{}\n", schema.trim_end()).as_bytes());
        }
        Action::Settings { command } => settings(&client, global, command)?,
        Action::Token { command } => token(&client, global, command)?,
        Action::WebUi {
            command: WebUi::Password { command },
        } => web_ui_password(&client, global, command)?,
        Action::Cli { command } => match command {
            Registration::Status => value(install::status()?)?,
            Registration::Install { directory } => value(install::install(directory.clone())?)?,
            Registration::Uninstall { directory } => {
                confirm(global, "Unregister the private-ai-proxy command?")?;
                value(install::uninstall(directory.clone())?)?
            }
        },
        Action::App {
            command: App::Open { web },
        } => open_app(&client, global, *web)?,
        Action::Doctor => {
            let report = doctor(&client);
            output::print(action, global, &report)?;
            return match report["errors"].as_object() {
                Some(errors) if !errors.is_empty() => {
                    Err("One or more diagnostic checks failed.".into())
                }
                _ => Ok(()),
            };
        }
        Action::Diagnostics { output } => {
            let path = new_export_path(output)?;
            client.call(rpc::ExportDiagnostics {
                path: export_path(&path)?,
            })?;
            json!({"exported": path})
        }
        Action::Completions { shell } => {
            let mut command = crate::Cli::command();
            let name = command.get_name().to_owned();
            let mut script = Vec::new();
            clap_complete::generate(*shell, &mut command, name, &mut script);
            return output::print_raw(&script);
        }
    };
    output::print(action, global, &value)
}

/// `status --watch`: the current state, then each change. Human output skips
/// states that render the same.
fn watch_status(global: &Global) -> Result<(), CallError> {
    let action = Action::Status { watch: true };
    let mut previous = None;
    let mut failure = None;
    let mut waiting = false;
    let watched = Client::watch_connection(|watched| {
        let state = match watched {
            Watched::State(state) => *state,
            Watched::Busy => {
                if !std::mem::replace(&mut waiting, true) {
                    eprintln!("private-ai-proxy: every event stream of the backend is in use; waiting for one to close");
                }
                return true;
            }
        };
        let text = match value(AppStateWire::from(state))
            .and_then(|state| output::render_line(&action, global, &state))
        {
            Ok(text) => text,
            Err(error) => {
                failure = Some(error);
                return false;
            }
        };
        if !global.json && previous.as_ref() == Some(&text) {
            return true;
        }
        let written = output::write_stdout(text.as_bytes());
        previous = Some(text);
        written.unwrap_or_else(|error| {
            failure = Some(error.into());
            false
        })
    });
    match failure {
        Some(error) => Err(error),
        None => Ok(watched?),
    }
}

fn status(client: &Client) -> Result<Value, CallError> {
    Ok(if client.is_running()? {
        json!({"backend": client.version()?, "gateway": AppStateWire::from(client.state()?)})
    } else {
        json!({"backend": null, "status": "not_running"})
    })
}

fn service(client: &Client, global: &Global, command: &Service) -> Result<Value, CallError> {
    match command {
        Service::Start => {
            Client::ensure_service()?;
            value(client.version()?)
        }
        Service::Stop => {
            if client.is_running()? {
                confirm(
                    global,
                    "Stop the backend and restore managed agent configurations?",
                )?;
                client.shutdown()?;
            }
            Ok(json!({"status": "stopped"}))
        }
        Service::Status => status(client),
    }
}

/// `start`: select the profile, then start protection and wait until it is
/// verified, fails, or `timeout` seconds pass.
fn start(client: &Client, profile: Option<&str>, timeout: u64) -> Result<Value, CallError> {
    Client::ensure_service()?;
    if let Some(profile) = profile {
        if client.state()?.active_profile_id != profile {
            client.call(rpc::ActivateProfile {
                profile_id: profile.into(),
            })?;
        }
    }
    let state = client.state()?;
    if profile.is_some_and(|profile| profile != state.active_profile_id) {
        return Err("Profile selection was changed by another client.".into());
    }
    let settled = |state: &AppState| !state.configuration_verification;
    if state.status == VerificationStatus::Verified && settled(&state) {
        return value(AppStateWire::from(state));
    }
    let started = if state.status == VerificationStatus::Verifying && settled(&state) {
        state
    } else {
        client
            .call(rpc::Start {
                config: state.config,
            })?
            .state
    };
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        let state = client.state()?;
        if state.session_id != started.session_id {
            return Err("The protection operation was superseded by another client.".into());
        }
        match state.status {
            VerificationStatus::Verified if settled(&state) => {
                return value(AppStateWire::from(state))
            }
            VerificationStatus::Verifying => {}
            _ => {
                return Err(
                    "Protection did not become verified. Inspect private-ai-proxy status.".into(),
                )
            }
        }
        if Instant::now() >= deadline {
            return Err("Verification wait timed out; the backend may still be verifying. Inspect private-ai-proxy status before retrying.".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

/// `stop --offline`: restores what `stop` restores when no backend runs, such
/// as before uninstalling after an update that was not relaunched.
fn restore_agents_offline(client: &Client) -> Result<Value, CallError> {
    const RUNNING: &str =
        "The backend is running; restore the agents through it with `stop` or `service stop`.";
    if client.is_running()? {
        return Err(RUNNING.into());
    }
    let helper = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| "Cannot locate the application executable")?
        .with_file_name(agent_bridge::agents::helper_binary_name());
    value(desktop_runtime::controller::restore_agents_offline(helper)?.ok_or(RUNNING)?)
}

fn profiles(client: &Client, global: &Global, command: &Profiles) -> Result<Value, CallError> {
    const SAVE: &str = "Save this profile? This selects it as active and may restart protection.";
    let find = |profiles: Vec<ConfidentialProfile>, id: &str| {
        profiles
            .into_iter()
            .find(|profile| profile.id == id)
            .ok_or("Profile not found")
    };
    match command {
        Profiles::List => value(client.state()?.profiles),
        Profiles::Show { id } => value(find(client.state()?.profiles, id)?),
        Profiles::Import { file } => {
            let backup = desktop_core::maintenance::ProfileBackup::read(file)?;
            confirm(
                global,
                "Import these unverified profile configurations without credentials?",
            )?;
            Client::ensure_service()?;
            value(client.call(rpc::ImportProfiles { backup })?)
        }
        Profiles::Export { output } => {
            let path = new_export_path(output)?;
            client.call(rpc::ExportProfiles {
                path: export_path(&path)?,
            })?;
            Ok(json!({"exported": path}))
        }
        Profiles::Login(options) => {
            confirm(global, "Sign in and save this account profile? This selects the profile and may reconnect active protection.")?;
            value(account::login(client, global, options)?)
        }
        Profiles::Add {
            id,
            name,
            url,
            provider,
            key_stdin,
            allow_development_os,
        } => {
            let profile = ConfidentialProfileInput {
                id: id.clone(),
                name: name.clone(),
                remote_url: url.clone(),
                provider: (*provider).into(),
            };
            // Validate before reading a credential or making a request.
            desktop_core::config::resolve_profile(profile.clone(), None)?;
            confirm(global, SAVE)?;
            Client::ensure_service()?;
            if client.state()?.profiles.iter().any(|saved| saved.id == *id) {
                return Err(
                    "Profile ID already exists. Use profiles verify to update its credential."
                        .into(),
                );
            }
            let key = read_key(global, *key_stdin)?;
            value(client.call(rpc::Verify {
                profile,
                require_production_os: !allow_development_os,
                key: Some(key),
            })?)
        }
        Profiles::Verify { id, key_stdin } => {
            let state = client.state()?;
            let profile = find(state.profiles, id)?;
            confirm(global, SAVE)?;
            let key = (*key_stdin || !profile.credential_saved)
                .then(|| read_key(global, *key_stdin))
                .transpose()?;
            value(client.call(rpc::Verify {
                profile: ConfidentialProfileInput {
                    id: profile.id,
                    name: profile.name,
                    provider: profile.provider,
                    remote_url: profile.remote_url,
                },
                require_production_os: state.config.require_production_os,
                key,
            })?)
        }
        Profiles::Edit {
            id,
            name,
            url,
            provider,
            key_stdin,
            allow_development_os,
            require_production_os,
        } => {
            let state = client.state()?;
            let saved = find(state.profiles, id)?;
            let profile = ConfidentialProfileInput {
                id: saved.id.clone(),
                name: name.clone().unwrap_or(saved.name),
                provider: provider.map_or(saved.provider, Into::into),
                remote_url: url.clone().unwrap_or_else(|| saved.remote_url.clone()),
            };
            let resolved = desktop_core::config::resolve_profile(profile.clone(), None)?;
            let target_changed =
                resolved.provider != saved.provider || resolved.remote_url != saved.remote_url;
            confirm(
                global,
                "Save these profile changes? This selects the profile as active and may restart protection.",
            )?;
            let key = (*key_stdin || target_changed || !saved.credential_saved)
                .then(|| read_key(global, *key_stdin))
                .transpose()?;
            value(client.call(rpc::Verify {
                profile,
                require_production_os: !allow_development_os
                    && (*require_production_os || state.config.require_production_os),
                key,
            })?)
        }
        Profiles::Use { id } => {
            confirm(global, "Switch the active profile?")?;
            value(client.call(rpc::ActivateProfile {
                profile_id: id.clone(),
            })?)
        }
        Profiles::Remove { id } => {
            confirm(global, "Delete this profile and its stored credential?")?;
            value(client.call(rpc::DeleteProfile {
                profile_id: id.clone(),
            })?)
        }
    }
}

fn agents(client: &Client, global: &Global, command: &Agents) -> Result<Value, CallError> {
    match command {
        Agents::List => value(client.call(rpc::ListAgents)?),
        Agents::Connect {
            id,
            model,
            dry_run,
            revision,
        } => agent_change(
            client,
            global,
            id,
            true,
            model.clone(),
            *dry_run,
            revision.as_deref(),
        ),
        Agents::Disconnect {
            id,
            dry_run,
            revision,
        } => agent_change(
            client,
            global,
            id,
            false,
            None,
            *dry_run,
            revision.as_deref(),
        ),
        Agents::DisconnectAll => {
            confirm(
                global,
                "Disconnect all managed agents and restore their configuration?",
            )?;
            let codex = desktop_core::agents::Agent::Codex.id();
            let codex_recorded = !global.json
                && client.call(rpc::ListAgents).is_ok_and(|agents| {
                    agents
                        .iter()
                        .any(|agent| agent.id == codex && agent.recorded)
                });
            let statuses = client.call(rpc::DisconnectAllAgents)?;
            if codex_recorded {
                agent_service_hint(client, global, codex);
            }
            value(statuses)
        }
    }
}

/// Connect or disconnect one agent: apply a previewed `revision`, or preview
/// the change, show it for confirmation and apply that preview.
fn agent_change(
    client: &Client,
    global: &Global,
    id: &str,
    connect: bool,
    model: Option<String>,
    dry_run: bool,
    revision: Option<&str>,
) -> Result<Value, CallError> {
    let options = ConnectOptions {
        default_model: model,
    };
    let revision = match revision {
        Some(revision) => {
            confirm(
                global,
                "Apply this previously previewed agent configuration revision?",
            )?;
            revision.to_string()
        }
        None => {
            let preview = client.call(rpc::PreviewAgent {
                agent_id: id.into(),
                connect,
                options: options.clone(),
            })?;
            if dry_run {
                return value(preview);
            }
            if !global.yes && global.interactive() {
                eprintln!("{}", output::details(&value(&preview)?));
            }
            confirm(global, "Apply these agent configuration changes?")?;
            preview.revision
        }
    };
    let status = client.call(rpc::ApplyAgent {
        agent_id: id.into(),
        connect,
        revision,
        options,
    })?;
    agent_service_hint(client, global, id);
    value(status)
}

/// Codex's background service keeps the settings it started with. The
/// desktop app offers to stop it; in a terminal, a restart keeps the
/// terminal's environment.
fn agent_service_hint(client: &Client, global: &Global, id: &str) {
    if global.json || id != desktop_core::agents::Agent::Codex.id() {
        return;
    }
    let running = client.call(rpc::AgentServiceRunning {
        agent_id: id.into(),
    });
    if running.unwrap_or(false) {
        eprintln!(
            "Codex's background service still has the previous settings. Run \"codex app-server \
             daemon restart\" to apply them; this stops running Codex sessions."
        );
    }
}

fn usage(client: &Client, global: &Global, command: &Usage) -> Result<Value, CallError> {
    let query = |filter: &UsageFilter, page: Option<&Pagination>| UsageQuery {
        agent: filter.agent.clone(),
        model: filter.model.clone(),
        session_id: filter.session.clone(),
        since: filter.since,
        until: filter.until,
        cursor: page.and_then(|page| page.cursor.clone()),
        limit: page.map(|page| page.limit as usize),
    };
    match command {
        Usage::List { filter, page } => value(client.call(rpc::QueryUsage {
            query: query(filter, Some(page)),
        })?),
        Usage::Show { id, .. } => value(client.call(rpc::GetUsageRecord {
            record_id: id.clone(),
        })?),
        Usage::Export { filter, output, .. } => {
            let path = new_export_path(output)?;
            let rows = client.call(rpc::ExportUsage {
                query: query(filter, None),
                path: export_path(&path)?,
            })?;
            Ok(json!({ "rows": rows }))
        }
        Usage::Clear => {
            confirm(global, "Permanently clear all usage history?")?;
            Ok(json!({"deleted": client.call(rpc::ClearUsage)?}))
        }
    }
}

fn settings(client: &Client, global: &Global, command: &Settings) -> Result<Value, CallError> {
    match command {
        Settings::Reset => {
            confirm(global, "Stop protection, restore all agents, and reset backend settings, including turning off the web UI? Profiles, keys and usage are kept. Open at Login is managed by the desktop app.")?;
            value(client.call(rpc::ResetSettings)?)
        }
        Settings::Show => {
            let state = client.state()?;
            let files = state.config_files;
            Ok(json!({
                "files": {
                    "config": files.config_path,
                    "credentials": files.credentials_path,
                    "error": files.error,
                    "warnings": files.warnings,
                },
                "settings": client.call(rpc::Settings)?,
                "webUi": state.web_ui,
            }))
        }
        Settings::Schema => unreachable!("printed without the backend"),
        Settings::Set {
            key,
            value: input,
            value_stdin,
        } => {
            if let Some(warning) = key.deprecation() {
                eprintln!("{warning}");
            }
            set_setting(global, client, key.key, input.as_deref(), *value_stdin)
        }
    }
}

/// `settings set`: one key, through the backend like the desktop Settings page.
fn set_setting(
    global: &Global,
    client: &Client,
    key: SettingsKey,
    input: Option<&str>,
    value_stdin: bool,
) -> Result<Value, CallError> {
    if key == SettingsKey::WebUiPassword {
        confirm(
            global,
            "Change the web UI password and sign out every browser?",
        )?;
        let password = read_web_ui_password(global, input, value_stdin)?;
        return value(client.call(rpc::SetWebUiPassword { password })?);
    }
    if value_stdin {
        return Err("Only web-ui.password reads its value from stdin".into());
    }
    let input = input.ok_or_else(|| format!("Missing the value for {key}"))?;
    confirm(global, "Change Private AI Proxy settings?")?;
    let port = || input.parse().map_err(|_| "Expected a valid port number");
    let client_host = || (!input.is_empty()).then(|| input.to_string());
    let change = match key {
        SettingsKey::AutoCliRegistration => Preference::AutoCliRegistration(parse_bool(input)?),
        SettingsKey::NotificationsEnabled
        | SettingsKey::NotificationsGateway
        | SettingsKey::NotificationsLocalApi
        | SettingsKey::NotificationsVerification => Preference::Notification {
            kind: match key {
                SettingsKey::NotificationsEnabled => NotificationKind::Enabled,
                SettingsKey::NotificationsGateway => NotificationKind::Gateway,
                SettingsKey::NotificationsLocalApi => NotificationKind::LocalApi,
                _ => NotificationKind::Verification,
            },
            enabled: parse_bool(input)?,
        },
        SettingsKey::Notifications => {
            Preference::Notifications(serde_json::from_str(input).map_err(|_| {
                "Expected notification settings as a JSON object with boolean values"
            })?)
        }
        SettingsKey::ConnectOnLaunch => Preference::ConnectOnLaunch(parse_bool(input)?),
        SettingsKey::Appearance => {
            Preference::Appearance(parse_name(input, "Expected system, light, or dark")?)
        }
        SettingsKey::UpdateChannel => {
            Preference::UpdateChannel(parse_name(input, "Expected beta or stable")?)
        }
        SettingsKey::WebUi
        | SettingsKey::WebUiPort
        | SettingsKey::WebUiListenAddress
        | SettingsKey::WebUiAllowNetworkAccess
        | SettingsKey::WebUiClientHost => {
            let mut config = client.call(rpc::Settings)?.web_ui;
            match key {
                SettingsKey::WebUi => config.enabled = parse_bool(input)?,
                SettingsKey::WebUiPort => config.port = port()?,
                SettingsKey::WebUiListenAddress => config.listen_address = input.to_string(),
                SettingsKey::WebUiAllowNetworkAccess => {
                    config.allow_network_access = parse_bool(input)?
                }
                _ => config.client_host = client_host(),
            }
            return value(client.call(rpc::SaveWebUi { config })?);
        }
        SettingsKey::ListenAddress
        | SettingsKey::AllowNetworkAccess
        | SettingsKey::Port
        | SettingsKey::ClientHost => {
            let mut config = client.state()?.local_api;
            match key {
                SettingsKey::ListenAddress => config.listen_address = input.to_string(),
                SettingsKey::AllowNetworkAccess => config.allow_network_access = parse_bool(input)?,
                SettingsKey::Port => config.port = port()?,
                _ => config.client_host = client_host(),
            }
            desktop_core::config::resolve_local_api(config.clone())?;
            return value(client.call(rpc::SaveLocalApiConfig { config })?);
        }
        SettingsKey::WebUiPassword => unreachable!("handled above"),
    };
    value(client.call(rpc::SetPreference { change })?)
}

fn token(client: &Client, global: &Global, command: &Token) -> Result<Value, CallError> {
    match command {
        Token::Rotate => {
            confirm(
                global,
                "Rotate the local API token and revoke the previous token?",
            )?;
            client.call(rpc::RotateClientKey)?;
            Ok(json!({"rotated": true}))
        }
        Token::Show => {
            confirm(global, "Reveal the local API token on stdout?")?;
            Ok(json!({"token": client.call(rpc::GetClientKey)?}))
        }
        Token::ClearCredential => {
            confirm(global, "Remove the active profile credential?")?;
            value(client.call(rpc::ClearApiKey)?)
        }
    }
}

fn web_ui_password(
    client: &Client,
    global: &Global,
    command: &WebUiPassword,
) -> Result<Value, CallError> {
    match command {
        WebUiPassword::Show => {
            confirm(global, "Reveal the web UI password on stdout?")?;
            let password = client.call(rpc::GetWebUiPassword)?.ok_or(
                "An earlier version saved only a hash of this password, so it cannot be shown. Run `pap web-ui password rotate` to replace it.",
            )?;
            Ok(json!({ "password": password }))
        }
        WebUiPassword::Rotate => {
            confirm(
                global,
                "Replace the web UI password and sign out every browser?",
            )?;
            client.call(rpc::RotateWebUiPassword)?;
            Ok(json!({ "rotated": true }))
        }
    }
}

/// `app open`: the desktop app, or the web UI where no desktop app or
/// graphical session exists or `--web` asks for it.
fn open_app(client: &Client, global: &Global, web: bool) -> Result<Value, CallError> {
    // Like the desktop app, the web path fails fast during installer updates
    // instead of waiting out the startup gate.
    let data = desktop_core::paths::app_data_dir()?;
    let startup = desktop_core::lock::startup_shared(&data)
        .map_err(|_| "Cannot acquire app startup lock")?
        .ok_or("Backend startup or an update is already in progress.")?;
    if let Some(app) = desktop_app().filter(|_| !web && graphical_session()) {
        let mut child = desktop_core::launch::child_command(app)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|_| "Cannot launch desktop UI")?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Ok(json!({"opened": true}));
    }
    // Backend startup takes the gate itself.
    drop(startup);
    open_web_ui(client, global)
}

fn desktop_app() -> Option<PathBuf> {
    let backend = desktop_core::launch::service_executable().ok()?;
    let app = backend.parent()?.join(if cfg!(windows) {
        "private-ai-proxy-desktop.exe"
    } else {
        "private-ai-proxy-desktop"
    });
    app.is_file().then_some(app)
}

/// Whether windows can appear for this user; remote shells get printed links instead.
fn graphical_session() -> bool {
    let set = |name: &str| std::env::var_os(name).is_some_and(|value| !value.is_empty());
    if set("SSH_CONNECTION") || set("SSH_TTY") {
        return false;
    }
    cfg!(any(target_os = "macos", windows)) || set("DISPLAY") || set("WAYLAND_DISPLAY")
}

/// Opens or prints the web UI address. It carries no secret: the page asks for
/// the web UI password.
fn open_web_ui(client: &Client, global: &Global) -> Result<Value, CallError> {
    Client::ensure_service()?;
    let mut status = client.state()?.web_ui;
    if !status.enabled {
        if !global.yes && !global.interactive() {
            return Err("The web UI is off. Enable it with `pap settings set web-ui.enabled true`, or rerun with --yes.".into());
        }
        confirm(
            global,
            &format!(
                "Web UI is off. Enable it on {}:{}?",
                desktop_core::listen::url_host(&status.listen_address),
                status.port
            ),
        )?;
        // The same change `settings set web-ui.enabled true` makes.
        let mut config = client.call(rpc::Settings)?.web_ui;
        config.enabled = true;
        status = client.call(rpc::SaveWebUi { config })?.state.web_ui;
    }
    let url = status.url.ok_or_else(|| {
        format!(
            "Web UI is not listening: {}",
            status
                .error
                .unwrap_or_else(|| "the listener is unavailable".into())
        )
    })?;
    let opened = graphical_session() && open_browser(&url).is_ok();
    Ok(json!({ "url": url, "browserOpened": opened }))
}

/// Open `url` in the user's browser without waiting for it. The launcher runs
/// detached with null stdio, so a text-mode fallback browser cannot take over
/// this shell.
fn open_browser(url: &str) -> Result<(), String> {
    open::that_detached(url).map_err(|_| "Cannot open a browser".to_string())
}

fn new_export_path(path: &Path) -> Result<PathBuf, String> {
    let path = std::path::absolute(path).map_err(|_| "Cannot resolve export path")?;
    if path.exists() {
        return Err("Export target already exists; choose a new path.".into());
    }
    Ok(path)
}

fn doctor(client: &Client) -> Value {
    let mut errors = Map::new();
    let backend_running = doctor_check(&mut errors, "backendRunning", client.is_running());
    let cli = doctor_check(&mut errors, "cli", install::status());
    let backend_executable = doctor_check(
        &mut errors,
        "backendExecutable",
        desktop_core::launch::service_executable(),
    );
    let endpoint = doctor_check(
        &mut errors,
        "endpoint",
        desktop_core::transport::endpoint_path()
            .map_err(|_| "Cannot resolve management endpoint".to_string()),
    );
    let logs = doctor_check(&mut errors, "logs", desktop_core::paths::logs_dir());
    let mut warnings = Map::new();
    let settings = settings_diagnostics(client, &mut errors, &mut warnings);
    // Update availability is advisory: an offline check never fails the doctor.
    let update =
        update_notice().map_or_else(|error| json!({ "error": error }), |notice| json!(notice));
    json!({
        "version": desktop_core::protocol::BUILD_VERSION,
        "update": update,
        "backendRunning": backend_running,
        "cli": cli,
        "backendExecutable": backend_executable,
        "endpoint": endpoint,
        "logs": logs,
        "settings": settings,
        "errors": errors,
        "warnings": warnings,
    })
}

/// The settings files as the running backend sees them, or as this user's
/// environment resolves them. An invalid file, or a 0.1 import the running
/// backend could not finish, fails the doctor; unknown keys, 0.1 import
/// notices, settings files left where macOS and Windows builds up to
/// 0.2.0-beta.8 kept them, a credentials file other users can read and profiles without a
/// saved key warn.
fn settings_diagnostics(
    client: &Client,
    errors: &mut Map<String, Value>,
    warnings: &mut Map<String, Value>,
) -> Value {
    let running = client.is_running().unwrap_or(false);
    let state = running.then(|| client.state().ok()).flatten();
    let (config, credentials, error, notices) = match &state {
        Some(state) => (
            state.config_files.config_path.clone(),
            state.config_files.credentials_path.clone(),
            state.config_files.error.clone(),
            state.config_files.warnings.clone(),
        ),
        None => {
            let paths = desktop_core::config::config_path()
                .and_then(|config| Ok((config, desktop_core::config::credentials_path()?)));
            let (config, credentials) = match paths {
                Ok(paths) => paths,
                Err(error) => {
                    errors.insert("settings".into(), json!(error));
                    return Value::Null;
                }
            };
            let loaded = match std::fs::read_to_string(&config) {
                Ok(text) => desktop_core::config::parse(&text).map(|parsed| parsed.unknown),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
                Err(error) => Err(format!("Cannot read {}: {error}", config.display())),
            };
            let (error, unknown) = match loaded {
                Ok(unknown) => (None, unknown),
                Err(error) => (Some(error), Vec::new()),
            };
            (
                config.to_string_lossy().into_owned(),
                credentials.to_string_lossy().into_owned(),
                error,
                unknown,
            )
        }
    };
    if let Some(error) = &error {
        errors.insert("settings".into(), json!(error));
    }
    if !notices.is_empty() {
        warnings.insert("settingsFiles".into(), json!(notices));
    }
    // Also without the backend, which reports it only when it starts.
    if let Some(leftover) = desktop_core::relocation::diagnostic() {
        warnings.insert("legacySettingsDirectory".into(), json!(leftover));
    }
    match desktop_core::private_fs::readable_by_others(Path::new(&credentials)) {
        Ok(Some(true)) => {
            let fix = if cfg!(windows) {
                format!("run `icacls \"{credentials}\" /inheritance:r /grant:r \"%USERNAME%:F\"`")
            } else {
                format!("run `chmod 600 {credentials}`")
            };
            warnings.insert(
                "credentials".into(),
                json!(format!("{credentials} can be read by other users; {fix}")),
            );
        }
        Ok(_) => {}
        Err(error) => {
            warnings.insert(
                "credentials".into(),
                json!(format!("Cannot check who can read {credentials}: {error}")),
            );
        }
    }
    if let Some(state) = &state {
        let missing: Vec<_> = state
            .profiles
            .iter()
            .filter(|profile| !profile.credential_saved)
            .map(|profile| profile.name.as_str())
            .collect();
        if !missing.is_empty() {
            warnings.insert(
                "profileCredentials".into(),
                json!(format!("No saved API key for: {}", missing.join(", "))),
            );
        }
    }
    json!({ "config": config, "credentials": credentials, "error": error, "warnings": notices })
}

fn update_notice() -> Result<desktop_core::updates::UpdateNotice, String> {
    // The CLI dispatches synchronously inside the async entry point.
    std::thread::spawn(|| {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "Could not check for updates".to_string())?
            .block_on(desktop_core::updates::check_installation())
    })
    .join()
    .map_err(|_| "Could not check for updates".to_string())?
}

fn doctor_check<T: Serialize>(
    errors: &mut Map<String, Value>,
    name: &str,
    result: Result<T, String>,
) -> Value {
    match result.and_then(|value| {
        serde_json::to_value(value).map_err(|_| "Cannot encode diagnostic result".into())
    }) {
        Ok(value) => value,
        Err(error) => {
            errors.insert(name.into(), json!(error));
            Value::Null
        }
    }
}

fn confirm(global: &Global, prompt: &str) -> Result<(), String> {
    if global.yes {
        return Ok(());
    }
    if !global.interactive() {
        return Err("Confirmation required. Pass --yes for noninteractive changes.".into());
    }
    eprint!("{prompt} [y/N] ");
    io::stderr()
        .flush()
        .map_err(|_| "Cannot write confirmation prompt")?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|_| "Cannot read confirmation")?;
    if answer.trim().eq_ignore_ascii_case("y") {
        Ok(())
    } else {
        Err("Cancelled".into())
    }
}

fn read_key(global: &Global, stdin: bool) -> Result<String, String> {
    if stdin {
        if io::stdin().is_terminal() {
            return Err(
                "Refusing to read a credential from a terminal with --key-stdin; omit the flag for a hidden prompt."
                    .into(),
            );
        }
        return private_ai_proxy::read_api_key(io::stdin());
    }
    if !global.interactive() {
        return Err("Use --key-stdin for noninteractive credential input".into());
    }
    let key = rpassword::prompt_password("API key: ").map_err(|_| "Cannot read credential")?;
    desktop_core::config::validate_api_key(&key)
}

/// Reads a new web UI password from stdin or a hidden prompt. A command-line
/// value is refused so it never reaches argv.
fn read_web_ui_password(
    global: &Global,
    input: Option<&str>,
    stdin: bool,
) -> Result<String, String> {
    if input.is_some() {
        return Err(
            "Pass the web UI password with --value-stdin or at the hidden prompt, not as an argument."
                .into(),
        );
    }
    if stdin {
        if io::stdin().is_terminal() {
            return Err("Refusing to read a password from a terminal with --value-stdin; omit the flag for a hidden prompt.".into());
        }
        let mut bytes = Vec::new();
        io::stdin()
            .take(4097)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read the password from stdin")?;
        if bytes.len() > 4096 {
            return Err("Password input exceeds limit".into());
        }
        let text = String::from_utf8(bytes).map_err(|_| "Password must be UTF-8")?;
        // The newline `echo` or a file adds is not part of the password.
        let text = text.strip_suffix('\n').unwrap_or(&text);
        return Ok(text.strip_suffix('\r').unwrap_or(text).to_string());
    }
    if !global.interactive() {
        return Err("Use --value-stdin for noninteractive password input".into());
    }
    rpassword::prompt_password("Web UI password: ").map_err(|_| "Cannot read the password".into())
}

fn parse_bool(value: &str) -> Result<bool, String> {
    value.parse().map_err(|_| "Expected true or false".into())
}

/// A settings value named by its serialized form, such as `dark`.
fn parse_name<T: DeserializeOwned>(value: &str, expected: &str) -> Result<T, String> {
    serde_json::from_value(Value::from(value)).map_err(|_| expected.into())
}

fn value(input: impl Serialize) -> Result<Value, CallError> {
    serde_json::to_value(input).map_err(|_| "Cannot encode output".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn automation_modes_never_prompt_or_imply_consent() {
        for mode in ["--json", "--non-interactive", "--no-interactive"] {
            let cli = crate::Cli::try_parse_from(["private-ai-proxy", mode, "status"]).unwrap();
            assert!(confirm(&cli.global, "Confirm?")
                .unwrap_err()
                .contains("--yes"));
            assert!(read_key(&cli.global, false)
                .unwrap_err()
                .contains("--key-stdin"));
            let approved =
                crate::Cli::try_parse_from(["private-ai-proxy", mode, "--yes", "status"]).unwrap();
            assert!(confirm(&approved.global, "Confirm?").is_ok());
        }
    }
}
