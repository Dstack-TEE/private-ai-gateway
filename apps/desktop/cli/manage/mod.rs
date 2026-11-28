//! The management commands, which drive the per-user backend through its
//! local API as the desktop app does.

use std::{
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use desktop_core::{
    client::{CallError, Client, Watched},
    contracts::*,
    protocol::{self, export_path, rpc, NotificationKind, Preference},
    supervisor::{self, AutomaticBlockers},
    usage::UsageQuery,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Map, Value};

use crate::Global;

mod account;
mod agent_paths;
mod args;
mod install;
mod output;

pub use args::Action;
use args::*;

pub fn has_profile_require_production_os(action: &Action) -> bool {
    matches!(
        action,
        Action::Profiles {
            command: Profiles::Edit {
                require_production_os: true,
                ..
            },
        }
    )
}

pub fn validate_arguments(action: &Action) -> Result<(), clap::Error> {
    if let Action::Settings {
        command:
            Settings::Set {
                key,
                value,
                value_stdin,
            },
    } = action
    {
        if key.key != SettingsKey::WebUiPassword {
            if *value_stdin {
                return Err(crate::invocation_command().0.error(
                    clap::error::ErrorKind::ArgumentConflict,
                    "Only web-ui.password reads its value from stdin",
                ));
            }
            if value.is_none() {
                return Err(crate::invocation_command().0.error(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    format!("Missing the value for {}", key.key),
                ));
            }
        }
    }
    Ok(())
}

pub fn run(global: &Global, action: &Action) -> Result<(), CallError> {
    warn_deprecated_os_flags(action);
    let client = Client::new();
    let value = match action {
        Action::Status { watch: true } => return watch_status(global),
        Action::Status { watch: false }
        | Action::Service {
            command: Service::Status,
        } => status(&client)?,
        Action::Service { command } => service(&client, global, command)?,
        Action::Start { profile, timeout } => {
            let mut state = start_with_supervision(&client, global, profile.as_deref(), *timeout)?;
            if let Ok(version) = client.version() {
                add_agent_path_warnings(&mut state, agent_paths::warnings(&client, &version));
            }
            state
        }
        Action::Stop { offline: false } => {
            if !client.is_running()? {
                return Err(
                    "Backend is not running. Run pap stop --offline to restore agents without it."
                        .into(),
                );
            }
            value(client.call(rpc::Stop)?)?
        }
        Action::Stop { offline: true } => restore_agents_offline(&client, false)?,
        Action::Profiles { command } => profiles(&client, global, command)?,
        Action::Agents { command } => agents(&client, global, command)?,
        Action::Models {
            command: Models::List { refresh },
        } => {
            if !client.is_running()? {
                return Err(
                    "Backend is not running. Run pap start to verify the service and enable protection."
                        .into(),
                );
            }
            let state = if *refresh {
                client.call(rpc::RefreshCatalog)?.state
            } else {
                client.state()?
            };
            value(state.catalog.ok_or(
                "No verified model catalog. Run pap start to start and verify protection.",
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
                confirm(global, "Unregister the pap command?")?;
                value(install::uninstall(directory.clone())?)?
            }
        },
        Action::App {
            command: App::Open { web },
        } => open_app(&client, global, *web)?,
        Action::Update => update(&client)?,
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
            let (mut command, name) = crate::invocation_command();
            let mut script = Vec::new();
            clap_complete::generate(*shell, &mut command, name, &mut script);
            return output::print_raw(&script);
        }
    };
    output::print(action, global, &value)
}

fn warn_deprecated_os_flags(action: &Action) {
    let warning = |flag: &str| {
        eprintln!(
            "warning: {flag} is deprecated and has no effect; clients no longer check the gateway OS image."
        );
    };
    let (allow_development_os, require_production_os) = match action {
        Action::Profiles {
            command:
                Profiles::Add {
                    allow_development_os,
                    ..
                },
        } => (*allow_development_os, false),
        Action::Profiles {
            command:
                Profiles::Edit {
                    allow_development_os,
                    require_production_os,
                    ..
                },
        } => (*allow_development_os, *require_production_os),
        _ => return,
    };
    if allow_development_os {
        warning("--allow-development-os");
    }
    if require_production_os {
        warning("--require-production-os");
    }
}

fn apply_supervision_notice(
    json_output: bool,
    state: &mut Value,
    notice: String,
) -> Option<String> {
    if json_output {
        if let Some(object) = state.as_object_mut() {
            object.insert("supervisionNotice".into(), json!(notice));
        }
        None
    } else {
        Some(notice)
    }
}

fn notice_if_unowned(blockers: AutomaticBlockers, owned_registration: bool) -> Option<String> {
    (!owned_registration)
        .then(|| supervisor::automatic_skip_notice(blockers))
        .flatten()
}

/// Move verified protection under the login service when eligible, then wait
/// for the session to resume there.
fn start_with_supervision(
    client: &Client,
    global: &Global,
    profile: Option<&str>,
    timeout: u64,
) -> Result<Value, CallError> {
    let mut state = start(client, profile, timeout)?;
    let blockers = supervisor::automatic_blockers();
    let notice = match blockers {
        AutomaticBlockers::Eligible => match supervisor::enable() {
            Ok(newly_enabled) => {
                let moved = client.handoff_to_supervisor()?;
                if moved {
                    state = start(client, profile, timeout)?;
                }
                if !global.json && (newly_enabled || moved) {
                    eprintln!("Protection now resumes at login and after a crash using the service manager's environment (pap service disable turns that off).");
                }
                None
            }
            Err(error) => Some(format!(
                "Could not enable the login service: {error}. Protection is running."
            )),
        },
        AutomaticBlockers::Environment(_) => notice_if_unowned(blockers, supervisor::enabled()),
        _ => None,
    };
    if let Some(notice) = notice {
        if let Some(notice) = apply_supervision_notice(global.json, &mut state, notice) {
            eprintln!("warning: {}", private_ai_proxy::safe(&notice));
        }
    }
    Ok(state)
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
                    eprintln!("pap: every event stream of the backend is in use; waiting for one to close");
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
    let mut status = if client.is_running()? {
        let version = client.version()?;
        let mut status =
            json!({"backend": version, "gateway": AppStateWire::from(client.state()?)});
        add_agent_path_warnings(&mut status, agent_paths::warnings(client, &version));
        status
    } else {
        json!({"backend": null, "status": "not_running"})
    };
    status["loginService"] = json!(login_service_status());
    Ok(status)
}

fn add_agent_path_warnings(status: &mut Value, warnings: Vec<String>) {
    if !warnings.is_empty() {
        status["agentPathWarnings"] = json!(warnings);
    }
}

fn login_service_status() -> supervisor::LoginServiceStatus {
    supervisor::login_service_status()
}

fn service(client: &Client, global: &Global, command: &Service) -> Result<Value, CallError> {
    match command {
        Service::Start => {
            let started = Client::start_service_if_needed()?;
            let mut result = value(client.version()?)?;
            if !global.json {
                result["alreadyRunning"] = json!(!started);
            }
            Ok(result)
        }
        Service::Stop => {
            let running = client.is_running()?;
            if running {
                confirm(
                    global,
                    "Stop the backend and restore managed agent configurations?",
                )?;
                client.shutdown()?;
            } else if !cfg!(all(target_os = "macos", feature = "mac-app-store")) {
                confirm(
                    global,
                    "End the saved protection session and restore managed agent configurations?",
                )?;
                restore_agents_offline(client, false)?;
            }
            let mut result = json!({"status": "stopped"});
            if !global.json {
                result["alreadyStopped"] = json!(!running);
            }
            Ok(result)
        }
        Service::Restart => {
            client.restart_service()?;
            value(client.version()?)
        }
        Service::Enable => {
            // The backend that runs now moves under the service manager, and
            // one that is not running starts there, like `systemctl enable --now`.
            supervisor::enable()?;
            client.handoff_to_supervisor()?;
            Client::ensure_service()?;
            Ok(json!({"startAtLogin": true}))
        }
        Service::Disable { for_uninstall } => {
            // Windows uses the app's login item and has no backend registration.
            if cfg!(windows) && !for_uninstall {
                return Ok(json!({"startAtLogin": false, "supported": false}));
            }
            if cfg!(windows) && *for_uninstall {
                if client.is_running()? {
                    confirm(
                        global,
                        "Stop the backend and remove PAP-created agent providers?",
                    )?;
                    client.shutdown_installed_for_uninstall()?;
                } else {
                    restore_agents_offline(client, true)?;
                }
                return Ok(json!({"startAtLogin": false}));
            }
            let disable = if *for_uninstall {
                supervisor::unregister
            } else {
                supervisor::disable
            };
            // Quit through IPC before booting out a launch agent, which would
            // otherwise preserve the session when it receives the signal.
            if supervisor::enabled() && client.is_running()? {
                if *for_uninstall {
                    client.shutdown_installed_for_uninstall()?;
                } else {
                    client.shutdown_installed()?;
                }
                disable()?;
            } else {
                disable()?;
                if client.is_running()? {
                    if *for_uninstall {
                        client.shutdown_installed_for_uninstall()?;
                    } else {
                        client.shutdown_installed()?;
                    }
                } else {
                    restore_agents_offline(client, *for_uninstall)?;
                }
            }
            Ok(json!({"startAtLogin": false}))
        }
        Service::Run => {
            // The service manager's command. A backend a command already
            // started is left to run: exiting cleanly is not restarted.
            if client.is_running()? {
                return Ok(Value::Null);
            }
            let backend = desktop_core::launch::service_executable()?;
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                let error = desktop_core::launch::child_command(&backend)
                    .arg("--supervised")
                    .exec();
                Err(format!("Cannot run {}: {error}", backend.display()).into())
            }
            #[cfg(not(unix))]
            Err(format!("Run {} directly", backend.display()).into())
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
    if state.profiles.is_empty() {
        return Err("No profile yet. Sign in with pap profiles login <id>, or add a service and key with pap profiles add.".into());
    }
    if !state.api_key_saved {
        if let Some(profile) = state
            .profiles
            .iter()
            .find(|p| p.id == state.active_profile_id)
        {
            return Err(missing_profile_credential(profile).into());
        }
        return Err(
            "The active profile has no saved credential. Inspect pap profiles list.".into(),
        );
    }
    let settled = |state: &AppState| !state.configuration_verification;
    if state.status == VerificationStatus::Verified && settled(&state) {
        return value(AppStateWire::from(state));
    }
    let started = if state.status == VerificationStatus::Verifying && settled(&state) {
        state
    } else {
        match client.call(rpc::Start {
            config: state.config,
        }) {
            Ok(started) => started.state,
            // A backend that just launched may be resuming the session itself.
            Err(error) => {
                let state = client.state()?;
                if !state.session_active || !settled(&state) {
                    return Err(error);
                }
                state
            }
        }
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
            _ => return Err(unverified(&state)),
        }
        if Instant::now() >= deadline {
            return Err("Verification wait timed out; the backend may still be verifying. Inspect pap status before retrying.".into());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn missing_profile_credential(profile: &ConfidentialProfile) -> String {
    let (credential, command) = if profile.auth.is_api_key() {
        ("API key", "verify")
    } else {
        ("account credential", "login")
    };
    format!(
        "No saved {credential} for {}. Run pap profiles {command} {}.",
        profile.name, profile.id
    )
}

/// Why `start` ended without verified protection: the backend's reason,
/// typed as a verification failure when the service did not pass.
fn unverified(state: &AppState) -> CallError {
    let Some(error) = &state.error else {
        return "Protection did not become verified. Inspect pap status.".into();
    };
    let mut message = format!("Protection did not become verified. {error}");
    if state.reconnecting {
        if !message.ends_with('.') {
            message.push('.');
        }
        message.push_str(" The backend retries in the background until pap stop.");
    }
    if state.verification_failed {
        CallError::Api(protocol::Error::new(
            protocol::ErrorCode::ServiceVerificationFailed,
            message,
        ))
    } else {
        message.into()
    }
}

/// `stop --offline`: restores what `stop` restores when no backend runs, such
/// as before uninstalling after an update that was not relaunched.
fn restore_agents_offline(client: &Client, purge_provider: bool) -> Result<Value, CallError> {
    const RUNNING: &str =
        "The backend is running; restore the agents through it with `stop` or `service stop`.";
    if client.is_running()? {
        return Err(RUNNING.into());
    }
    let helper = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| "Cannot locate the application executable")?
        .with_file_name(agent_bridge::agents::helper_binary_name());
    value(
        desktop_runtime::controller::restore_agents_offline(helper, purge_provider)?
            .ok_or(RUNNING)?,
    )
}

fn profiles(client: &Client, global: &Global, command: &Profiles) -> Result<Value, CallError> {
    const SAVE: &str = "Save this profile? This selects it as active and may restart protection.";
    const DUPLICATE_ID: &str =
        "Profile ID already exists. Use pap profiles verify to update its credential.";
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
            ..
        } => {
            let default_provider = if url.is_some() {
                Provider::Custom
            } else {
                Provider::Redpill
            };
            let provider = provider.unwrap_or(default_provider).into();
            let profile = ConfidentialProfileInput {
                id: id.clone(),
                name: name.clone(),
                remote_url: profile_url(provider, url.clone(), None)?,
                provider,
            };
            // Validate before reading a credential or making a request.
            desktop_core::config::resolve_profile(profile.clone(), None)?;
            if desktop_core::config::load()?.profiles.contains_key(id) {
                return Err(DUPLICATE_ID.into());
            }
            confirm(global, SAVE)?;
            Client::ensure_service()?;
            if client.state()?.profiles.iter().any(|saved| saved.id == *id) {
                return Err(DUPLICATE_ID.into());
            }
            let key = read_key(global, *key_stdin)?;
            value(client.call(rpc::Verify {
                profile,
                key: Some(key),
            })?)
        }
        Profiles::Verify {
            id,
            key_stdin,
            replace_key,
        } => {
            let state = client.state()?;
            let protecting = state.session_active;
            let profile = find(state.profiles, id)?;
            // A new key restarts protection; the profile that is protecting,
            // unchanged, is verified where it runs.
            let replacing = *key_stdin || *replace_key || !profile.credential_saved;
            let unchanged = protecting && !replacing && state.active_profile_id == *id;
            confirm(
                global,
                if unchanged {
                    "Verify this profile's service again? Protection keeps running."
                } else if protecting {
                    "Protection is on. This restarts it, which ends requests in flight. Save this profile and select it as active?"
                } else {
                    SAVE
                },
            )?;
            let key = (*key_stdin || *replace_key || !profile.credential_saved)
                .then(|| read_key(global, *key_stdin))
                .transpose()?;
            let saved = client.call(rpc::Verify {
                profile: ConfidentialProfileInput {
                    id: profile.id,
                    name: profile.name,
                    provider: profile.provider,
                    remote_url: profile.remote_url,
                },
                key,
            })?;
            // Report the session that resumes, not the moment it restarts.
            let deadline = Instant::now() + Duration::from_secs(90);
            let mut state = saved.state;
            while protecting
                && state.status != VerificationStatus::Verified
                && (state.status == VerificationStatus::Verifying || state.reconnecting)
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(200));
                state = client.state()?;
            }
            if protecting && state.status != VerificationStatus::Verified {
                return Err(unverified(&state));
            }
            value(AppStateWire::from(state))
        }
        Profiles::Edit {
            id,
            name,
            url,
            provider,
            key_stdin,
            ..
        } => {
            let state = client.state()?;
            let saved = find(state.profiles, id)?;
            let provider = provider.map_or(saved.provider, Into::into);
            // A custom endpoint carries over only between custom profiles.
            let kept_url = (provider.preset_url().is_none()
                && saved.provider.preset_url().is_none())
            .then(|| saved.remote_url.clone());
            let profile = ConfidentialProfileInput {
                id: saved.id.clone(),
                name: name.clone().unwrap_or(saved.name),
                provider,
                remote_url: profile_url(provider, url.clone(), kept_url)?,
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
            value(client.call(rpc::Verify { profile, key })?)
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

/// The endpoint for a profile: Phala and RedPill always use their service URL,
/// as in the desktop app, and a custom provider needs one.
fn profile_url(
    provider: desktop_core::contracts::ServiceProvider,
    url: Option<String>,
    kept: Option<String>,
) -> Result<String, String> {
    url.or_else(|| provider.preset_url().map(str::to_owned))
        .or(kept)
        .ok_or_else(|| {
            "A custom provider needs --url. RedPill and Phala profiles use their service URL: pass --provider redpill or --provider phala.".into()
        })
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
    let agent = desktop_core::agents::Agent::from_id(id)?;
    let running = connect && client.is_running()?;
    let mut checked_backend = if running {
        Some(agent_paths::check(client, agent)?)
    } else {
        None
    };
    let options = ConnectOptions {
        default_model: model,
        ..ConnectOptions::default()
    };
    if dry_run {
        let request = rpc::PreviewAgent {
            agent_id: id.into(),
            connect,
            options,
        };
        return value(if let Some(version) = &checked_backend {
            agent_paths::warn_if_unreported(version);
            client.call_in_instance(request, &version.instance_id)?
        } else {
            client.call(request)?
        });
    }
    // The preview needs the verified catalog. Get consent to start it before
    // presenting or applying any agent configuration changes.
    let supervision_notice = if connect && (!running || !client.state()?.is_protected()) {
        confirm(global, "Start protection before connecting this agent?")?;
        let state = start_with_supervision(client, global, None, 90)?;
        checked_backend = Some(agent_paths::check(client, agent)?);
        state.get("supervisionNotice").cloned()
    } else {
        None
    };
    if let Some(version) = &checked_backend {
        agent_paths::warn_if_unreported(version);
    }
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
            if !global.yes && global.interactive() {
                eprintln!("{}", output::details(&value(&preview)?));
            }
            confirm(global, "Apply these agent configuration changes?")?;
            preview.revision
        }
    };
    let request = rpc::ApplyAgent {
        agent_id: id.into(),
        connect,
        revision,
        options,
    };
    let status = if let Some(version) = &checked_backend {
        client.call_in_instance(request, &version.instance_id)?
    } else {
        client.call(request)?
    };
    // Success would mean the agent is routed; it still uses its own provider.
    if connect && status.recorded && !status.connected {
        let next = if client.state()?.session_active {
            "protection is not verified yet; it is applied once it is (pap status shows why)"
        } else {
            "protection is off; run pap start to route it through the Local API"
        };
        return Err(format!(
            "Saved, but not applied, so {} still uses its own provider: {next}.",
            status.name
        )
        .into());
    }
    agent_service_hint(client, global, id);
    let mut result = value(status)?;
    if let Some(notice) = supervision_notice {
        result["supervisionNotice"] = notice;
    }
    Ok(result)
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
            confirm_with_hint(
                global,
                "Reveal the Local API token on stdout?",
                "Confirmation required. Pass --yes to reveal the Local API token noninteractively.",
            )?;
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
    if backend_running == true {
        if let Ok(version) = client.version() {
            let paths = agent_paths::warnings(client, &version);
            if !paths.is_empty() {
                warnings.insert("agentPaths".into(), json!(paths));
            }
        }
    }
    let settings = settings_diagnostics(client, &mut errors, &mut warnings);
    // Update availability is advisory: an offline check never fails the doctor.
    let update =
        update_notice().map_or_else(|error| json!({ "error": error }), |notice| json!(notice));
    json!({
        "version": desktop_core::protocol::BUILD_VERSION,
        "update": update,
        "backendRunning": backend_running,
        "loginService": login_service_status(),
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
            .map(missing_profile_credential)
            .collect();
        if !missing.is_empty() {
            warnings.insert("profileCredentials".into(), json!(missing.join(" ")));
        }
    }
    json!({ "config": config, "credentials": credentials, "error": error, "warnings": notices })
}

/// `update`: runs the upgrade steps `doctor` shows for this installation.
/// Like other self-updating CLIs it does not ask first: the command is the
/// request, and each step is printed as it runs. The steps stop the backend,
/// so protection that was on is started again by the installed release.
fn update(client: &Client) -> Result<Value, CallError> {
    use desktop_core::updates::{self, Installation};

    let installation = updates::installation();
    if let Some(notice) = updates::linux_repo_update(installation) {
        return value(notice);
    }
    if let Some(package_url) = installation.aur_package_url() {
        return Ok(json!({ "packageUrl": package_url }));
    }
    if let Some(notice) = installation.winget_update() {
        return value(notice);
    }
    let notice = update_notice()?;
    if notice.installation.delegates_updates() && !notice.commands.is_empty() {
        return value(notice);
    }
    let Some(version) = &notice.version else {
        return value(notice);
    };
    // A running Windows executable cannot be replaced.
    if cfg!(windows) || notice.commands.is_empty() {
        let how = match &notice.download_url {
            _ if !notice.commands.is_empty() => {
                format!("Run:\n  {}", notice.commands.join("\n  "))
            }
            Some(url) => format!("Download {url} and extract it into a fresh directory."),
            None => "Update it in the desktop app (Settings > About) or with its package manager."
                .into(),
        };
        return Err(format!(
            "{version} is available, but this installation does not update from this command. {how}"
        )
        .into());
    }
    // The manual steps stop the backend for good, which restores connected
    // agents to their own provider. An update stops it as for a restart
    // instead: agents stay pointed at the Local API, which nothing answers
    // meanwhile, the session waits, and no other command starts a backend
    // during the installation. If it fails, the previous release resumes.
    let (command, steps) = if notice.installation == Installation::InstallScript {
        updates::install_script_update(version)?
    } else {
        let command = std::env::current_exe()
            .map_err(|error| format!("Cannot locate this command: {error}"))?;
        let steps = notice
            .commands
            .iter()
            .filter(|step| *step != updates::STOP_STEP)
            .cloned()
            .collect();
        (command, steps)
    };
    let command = command.to_str().ok_or("This command's path is not UTF-8")?;
    let running = client.is_running()?;
    let protecting = running && client.state()?.session_active;
    client
        .install_update(|| updates::run(&steps))
        .map_err(|error| format!("The update was not installed: {error}"))?;
    // The installed release takes the session up again.
    if running {
        let resume: &[&str] = if protecting {
            &["start"]
        } else {
            &["service", "start"]
        };
        let step = updates::step(&[&[command], resume].concat())?;
        updates::run(&[step]).map_err(|error| {
            format!("Updated to {version}, but the backend did not start again: {error}.")
        })?;
    }
    let mut updated = value(&notice)?;
    updated["protectionResumed"] = json!(protecting);
    Ok(updated)
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
    confirm_with_hint(
        global,
        prompt,
        "Confirmation required. Pass --yes for noninteractive changes.",
    )
}

fn confirm_with_hint(global: &Global, prompt: &str, hint: &str) -> Result<(), String> {
    if global.yes {
        return Ok(());
    }
    if !global.interactive() {
        return Err(hint.into());
    }
    eprint!("{prompt} [y/N] ");
    io::stderr()
        .flush()
        .map_err(|_| "Cannot write confirmation prompt")?;
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|_| "Cannot read confirmation")?;
    if affirmative(&answer) {
        Ok(())
    } else {
        Err("Cancelled".into())
    }
}

fn affirmative(answer: &str) -> bool {
    let answer = answer.trim();
    answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes")
}

fn read_key(global: &Global, stdin: bool) -> Result<String, String> {
    if stdin {
        if io::stdin().is_terminal() {
            return Err(
                "Refusing to read a credential from a terminal with --key-stdin; pipe it in, or use `pap profiles verify <id> --replace-key` for a hidden prompt."
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
    fn preset_providers_use_their_service_url_and_custom_needs_one() {
        use desktop_core::contracts::ServiceProvider::{Custom, Redpill};
        let preset = Redpill.preset_url().map(str::to_owned);
        let custom = "https://gateway.example".to_string();
        let url = |provider, url: Option<&String>, kept: Option<&String>| {
            profile_url(provider, url.cloned(), kept.cloned()).ok()
        };
        assert_eq!(url(Redpill, None, None), preset);
        assert_eq!(url(Redpill, None, Some(&custom)), preset);
        assert_eq!(url(Custom, None, Some(&custom)), Some(custom.clone()));
        assert_eq!(url(Custom, Some(&custom), None), Some(custom.clone()));
        assert_eq!(url(Custom, None, None), None);
    }

    #[test]
    fn confirmations_accept_y_and_yes_without_case_sensitivity() {
        for answer in ["y", "Y", "yes", "YES", " YeS "] {
            assert!(affirmative(answer));
        }
        for answer in ["", "n", "no", "yesterday"] {
            assert!(!affirmative(answer));
        }
    }

    #[test]
    fn a_failed_verification_fails_start_with_its_reason() {
        let mut state = AppState {
            status: VerificationStatus::Error,
            reconnecting: true,
            verification_failed: true,
            error: Some("Service verification failed: id-6 … protection stays off.".into()),
            ..AppState::default()
        };
        let CallError::Api(error) = unverified(&state) else {
            panic!("a failed check is a typed API error");
        };
        assert_eq!(error.code, protocol::ErrorCode::ServiceVerificationFailed);
        assert_eq!(
            error.message,
            "Protection did not become verified. Service verification failed: id-6 … protection stays off. The backend retries in the background until pap stop."
        );
        // A connection failure is not a verification result.
        state.verification_failed = false;
        assert!(matches!(unverified(&state), CallError::Local(_)));
    }

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

    #[test]
    fn start_supervision_notice_uses_stderr_for_human_output_and_json_for_machine_output() {
        let notice = "Automatic supervision was skipped because CODEX_HOME is set. Run `pap service enable` to use the service manager's environment; configure those variables there.";
        let mut json_state = json!({"status": "verified"});
        assert!(apply_supervision_notice(true, &mut json_state, notice.into()).is_none());
        assert_eq!(json_state["supervisionNotice"], notice);

        let mut human_state = json!({"status": "verified"});
        assert_eq!(
            apply_supervision_notice(false, &mut human_state, notice.into()).as_deref(),
            Some(notice)
        );
        assert!(human_state.get("supervisionNotice").is_none());
    }

    #[test]
    fn owned_supervision_does_not_emit_a_skip_notice() {
        let blockers =
            desktop_core::supervisor::AutomaticBlockers::Environment(vec!["HTTPS_PROXY"]);
        assert!(notice_if_unowned(blockers, true).is_none());
    }

    #[test]
    fn missing_credentials_point_to_the_profiles_auth_method() {
        let mut profile = desktop_core::config::resolve_profile(
            ConfidentialProfileInput {
                id: "work".into(),
                name: "Work".into(),
                provider: ServiceProvider::Redpill,
                remote_url: ServiceProvider::Redpill.preset_url().unwrap().into(),
            },
            None,
        )
        .unwrap();
        assert!(missing_profile_credential(&profile).contains("pap profiles verify work"));
        profile.auth = ProfileAuth::OAuth {
            account_id: "account".into(),
            account_name: None,
            images: None,
            scope: None,
            funding: None,
        };
        assert!(missing_profile_credential(&profile).contains("pap profiles login work"));
    }
}
