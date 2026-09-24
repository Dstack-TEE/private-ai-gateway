use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use clap::{CommandFactory, FromArgMatches};
use desktop_core::{
    client::Client,
    contracts::*,
    preferences::{Appearance, UpdateChannel},
    protocol::{export_path, rpc, Preference},
    usage::UsageQuery,
};
use serde::Serialize;
use serde_json::{json, Map, Value};

mod account;
mod args;
mod install;
mod output;
mod schema;

use args::*;

pub fn cli_command() -> clap::Command {
    Cli::command()
}

pub fn run_matches(matches: &clap::ArgMatches, command: clap::Command) -> Result<(), String> {
    let cli = Cli::from_arg_matches(matches).map_err(|error| error.to_string())?;
    execute(&cli, command)
}

fn execute(cli: &Cli, mut command: clap::Command) -> Result<(), String> {
    match &cli.command {
        Action::Completions { shell } => {
            let name = command.get_name().to_owned();
            let mut completion = Vec::new();
            clap_complete::generate(*shell, &mut command, name, &mut completion);
            return finish_output(write_bytes(&completion));
        }
        Action::Schema => {
            command.build();
            return finish_output(write_text(
                &serde_json::to_string(&schema::command(&command))
                    .map_err(|_| "Cannot encode command schema")?,
            ));
        }
        _ => {}
    }
    let client = Client::new();
    let result = match &cli.command {
        Action::Status { watch: true } => {
            let mut previous = None;
            let mut output_error = None;
            let watched = Client::watch_connection(|state| {
                let text = match render_output(&state, cli) {
                    Ok(text) => text,
                    Err(error) => {
                        output_error = Some(OutputError::Message(error));
                        return false;
                    }
                };
                if !cli.json && previous.as_ref() == Some(&text) {
                    return true;
                }
                previous = Some(text.clone());
                match write_text(&text) {
                    Ok(()) => true,
                    Err(error) => {
                        output_error = Some(error);
                        false
                    }
                }
            });
            return match output_error {
                Some(error) => finish_output(Err(error)),
                None => watched,
            };
        }
        Action::Status { watch: false }
        | Action::Service {
            command: Service::Status,
        } => {
            if client.is_running()? {
                json!({"backend": client.hello()?, "gateway": client.state()?})
            } else {
                json!({"backend": null, "status": "not_running"})
            }
        }
        Action::Service {
            command: Service::Start,
        } => {
            Client::ensure_service()?;
            value(client.hello()?)?
        }
        Action::Service {
            command: Service::Stop,
        } => {
            if client.is_running()? {
                confirm(
                    cli,
                    "Stop the backend and restore managed agent configurations?",
                )?;
                client.shutdown()?;
            }
            json!({"status": "stopped"})
        }
        Action::Start { profile, timeout } => {
            Client::ensure_service()?;
            if let Some(profile) = profile {
                if client.state()?.active_profile_id != *profile {
                    client.call(rpc::ActivateProfile {
                        profile_id: profile.clone(),
                    })?;
                }
            }
            let state = client.state()?;
            if profile
                .as_ref()
                .is_some_and(|profile| profile != &state.active_profile_id)
            {
                return Err("Profile selection was changed by another client.".into());
            }
            if state.status == "verified" && !state.configuration_verification {
                value(state)?
            } else {
                let started = if state.status == "verifying" && !state.configuration_verification {
                    state
                } else {
                    client.call(rpc::Start {
                        config: state.config,
                    })?
                };
                let deadline = Instant::now() + Duration::from_secs(*timeout);
                loop {
                    let state = client.state()?;
                    if state.session_id != started.session_id {
                        return Err(
                            "The protection operation was superseded by another client.".into()
                        );
                    }
                    match state.status.as_str() {
                        "verified" if !state.configuration_verification => break value(state)?,
                        "verifying" => {}
                        _ => return Err(
                            "Protection did not become verified. Inspect private-ai-proxy status."
                                .into(),
                        ),
                    }
                    if Instant::now() >= deadline {
                        return Err("Verification wait timed out; the backend may still be verifying. Inspect private-ai-proxy status before retrying.".into());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        Action::Stop => value(client.call(rpc::Stop)?)?,
        Action::Profiles { command } => {
            match command {
                Profiles::Login(options) => {
                    confirm(cli, "Sign in and save this account profile? This selects the profile and may reconnect active protection.")?;
                    account::login(&client, cli, options)?
                }
                Profiles::List => value(client.state()?.profiles)?,
                Profiles::Show { id } => value(
                    client
                        .state()?
                        .profiles
                        .into_iter()
                        .find(|profile| profile.id == *id)
                        .ok_or("Profile not found")?,
                )?,
                Profiles::Import { file } => {
                    let backup = desktop_core::maintenance::ProfileBackup::read(file)?;
                    confirm(
                        cli,
                        "Import these unverified profile configurations without credentials?",
                    )?;
                    Client::ensure_service()?;
                    value(client.call(rpc::ImportProfiles { backup })?)?
                }
                Profiles::Export { output } => {
                    let path = new_export_path(output)?;
                    client.call(rpc::ExportProfiles {
                        path: export_path(&path)?,
                    })?;
                    json!({"exported": path})
                }
                Profiles::Use { id } => {
                    confirm(cli, "Switch the active profile?")?;
                    value(client.call(rpc::ActivateProfile {
                        profile_id: id.clone(),
                    })?)?
                }
                Profiles::Remove { id } => {
                    confirm(cli, "Delete this profile and its stored credential?")?;
                    value(client.call(rpc::DeleteProfile {
                        profile_id: id.clone(),
                    })?)?
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
                        provider: service_provider(*provider),
                    };
                    // Validate before reading a credential or making a request.
                    desktop_core::service_config::resolve_profile(profile.clone(), None)?;
                    confirm(
                        cli,
                        "Save this profile? This selects it as active and may restart protection.",
                    )?;
                    Client::ensure_service()?;
                    if client.state()?.profiles.iter().any(|saved| saved.id == *id) {
                        return Err("Profile ID already exists. Use profiles verify to update its credential.".into());
                    }
                    let key = read_key(cli, *key_stdin)?;
                    value(client.call(rpc::Verify {
                        profile,
                        require_production_os: !allow_development_os,
                        key: Some(key),
                    })?)?
                }
                Profiles::Verify { id, key_stdin } => {
                    let state = client.state()?;
                    let profile = state
                        .profiles
                        .into_iter()
                        .find(|profile| profile.id == *id)
                        .ok_or("Profile not found")?;
                    confirm(
                        cli,
                        "Save this profile? This selects it as active and may restart protection.",
                    )?;
                    let key = if *key_stdin || !profile.credential_saved {
                        Some(read_key(cli, *key_stdin)?)
                    } else {
                        None
                    };
                    value(client.call(rpc::Verify {
                        profile: ConfidentialProfileInput {
                            id: profile.id,
                            name: profile.name,
                            provider: profile.provider,
                            remote_url: profile.remote_url,
                        },
                        require_production_os: state.config.require_production_os,
                        key,
                    })?)?
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
                    let saved = state
                        .profiles
                        .iter()
                        .find(|profile| profile.id == *id)
                        .ok_or("Profile not found")?;
                    let profile = ConfidentialProfileInput {
                        id: saved.id.clone(),
                        name: name.clone().unwrap_or_else(|| saved.name.clone()),
                        provider: provider.map(service_provider).unwrap_or(saved.provider),
                        remote_url: url.clone().unwrap_or_else(|| saved.remote_url.clone()),
                    };
                    let resolved =
                        desktop_core::service_config::resolve_profile(profile.clone(), None)?;
                    let target_changed = resolved.provider != saved.provider
                        || resolved.remote_url != saved.remote_url;
                    let credential_saved = saved.credential_saved;
                    confirm(
                        cli,
                        "Save these profile changes? This selects the profile as active and may restart protection.",
                    )?;
                    let key = if *key_stdin || target_changed || !credential_saved {
                        Some(read_key(cli, *key_stdin)?)
                    } else {
                        None
                    };
                    let production_os = if *allow_development_os {
                        false
                    } else if *require_production_os {
                        true
                    } else {
                        state.config.require_production_os
                    };
                    value(client.call(rpc::Verify {
                        profile,
                        require_production_os: production_os,
                        key,
                    })?)?
                }
            }
        }
        Action::Agents { command } => match command {
            Agents::List => value(client.call(rpc::Agents)?)?,
            Agents::Connect {
                id,
                model,
                dry_run,
                revision,
            } => agent_change(
                &client,
                cli,
                id,
                true,
                model.clone(),
                *dry_run,
                revision.as_deref(),
            )?,
            Agents::Disconnect {
                id,
                dry_run,
                revision,
            } => agent_change(&client, cli, id, false, None, *dry_run, revision.as_deref())?,
            Agents::DisconnectAll => {
                confirm(
                    cli,
                    "Disconnect all managed agents and restore their configuration?",
                )?;
                value(client.call(rpc::DisconnectAllAgents)?)?
            }
        },
        Action::Models {
            command: Models::List { refresh },
        } => {
            let state: AppState = if *refresh {
                client.call(rpc::RefreshCatalog)?
            } else {
                client.state()?
            };
            value(state.catalog.ok_or(
                "No verified model catalog. Start protection and wait for verification first.",
            )?)?
        }
        Action::Usage { command } => match command {
            Usage::List { filter, page } => value(client.call(rpc::Usage {
                query: query(filter, Some(page)),
            })?)?,
            Usage::Show { id } => value(
                client
                    .call(rpc::UsageRecord {
                        record_id: id.clone(),
                    })?
                    .ok_or("Usage record not found")?,
            )?,
            Usage::Export { filter, output, .. } => {
                let path = std::path::absolute(output).map_err(|_| "Cannot resolve export path")?;
                if path.exists() {
                    return Err("Export target already exists; choose a new path.".into());
                }
                let rows = client.call(rpc::ExportUsage {
                    query: query(filter, None),
                    path: export_path(&path)?,
                })?;
                value(json!({ "rows": rows }))?
            }
            Usage::Clear => {
                confirm(cli, "Permanently clear all usage history?")?;
                json!({"deleted": client.call(rpc::ClearUsage)?})
            }
        },
        Action::Settings { command } => {
            match command {
                Settings::Reset => {
                    confirm(cli, "Stop protection, restore all agents, and reset backend settings, including turning off the web UI? Profiles, keys and usage are kept. Open at Login is managed by the desktop app.")?;
                    value(client.call(rpc::ResetSettings)?)?
                }
                Settings::Show => {
                    let state = client.state()?;
                    json!({"preferences": client.call(rpc::Preferences)?, "localApi": state.local_api, "webUi": state.web_ui})
                }
                Settings::Set { key, value: input } => {
                    confirm(cli, "Change Private AI Proxy settings?")?;
                    let set = |change| client.call(rpc::SetPreference { change });
                    match key {
                    SettingsKey::AutoCliRegistration => value(
                        set(Preference::AutoCliRegistration(parse_bool(input)?))?,
                    )?,
                    SettingsKey::Notifications => value(set(Preference::Notifications(
                        serde_json::from_str(input).map_err(|_| "Expected notification settings as a JSON object with boolean values")?
                    ))?)?,
                    SettingsKey::ConnectOnLaunch => value(
                        set(Preference::ConnectOnLaunch(parse_bool(input)?))?,
                    )?,
                    SettingsKey::Appearance => value(set(Preference::Appearance(
                        match input.as_str() {
                            "system" => Appearance::System,
                            "light" => Appearance::Light,
                            "dark" => Appearance::Dark,
                            _ => return Err("Expected system, light, or dark".into()),
                        },
                    ))?)?,
                    SettingsKey::UpdateChannel => value(set(Preference::UpdateChannel(
                        match input.as_str() {
                            "beta" => UpdateChannel::Beta,
                            "stable" => UpdateChannel::Stable,
                            _ => return Err("Expected beta or stable".into()),
                        },
                    ))?)?,
                    SettingsKey::WebUi
                    | SettingsKey::WebUiPort
                    | SettingsKey::WebUiListenAddress
                    | SettingsKey::WebUiAllowNetworkAccess
                    | SettingsKey::WebUiClientHost => {
                        let mut config = client.call(rpc::Preferences)?.web_ui;
                        match key {
                            SettingsKey::WebUi => config.enabled = parse_bool(input)?,
                            SettingsKey::WebUiPort => {
                                config.port =
                                    input.parse().map_err(|_| "Expected a valid port number")?
                            }
                            SettingsKey::WebUiListenAddress => config.listen_address = input.clone(),
                            SettingsKey::WebUiAllowNetworkAccess => {
                                config.allow_network_access = parse_bool(input)?
                            }
                            SettingsKey::WebUiClientHost => {
                                config.client_host = (!input.is_empty()).then(|| input.clone())
                            }
                            _ => unreachable!(),
                        }
                        value(client.call(rpc::SaveWebUi { config })?)?
                    }
                    SettingsKey::ListenAddress
                    | SettingsKey::AllowNetworkAccess
                    | SettingsKey::Port
                    | SettingsKey::ClientHost => {
                        let mut config = client.state()?.local_api;
                        match key {
                            SettingsKey::ListenAddress => config.listen_address = input.clone(),
                            SettingsKey::AllowNetworkAccess => {
                                config.allow_network_access = parse_bool(input)?
                            }
                            SettingsKey::Port => {
                                config.port =
                                    input.parse().map_err(|_| "Expected a valid port number")?
                            }
                            SettingsKey::ClientHost => {
                                config.client_host = (!input.is_empty()).then(|| input.clone())
                            }
                            _ => unreachable!(),
                        }
                        desktop_core::local_api::resolve(config.clone())?;
                        value(client.call(rpc::SaveLocalApi { config })?)?
                    }
                }
                }
            }
        }
        Action::Token { command } => match command {
            Token::Rotate => {
                confirm(
                    cli,
                    "Rotate the local API token and revoke the previous token?",
                )?;
                client.call(rpc::RotateClientKey)?;
                json!({"rotated": true})
            }
            Token::Show => {
                confirm(cli, "Reveal the local API token on stdout?")?;
                json!({"token": client.call(rpc::ClientKey)?})
            }
            Token::ClearCredential => {
                confirm(cli, "Remove the active profile credential?")?;
                value(client.call(rpc::ClearApiKey)?)?
            }
        },
        Action::Cli { command } => match command {
            Registration::Status => value(install::status()?)?,
            Registration::Install { directory } => value(install::install(directory.clone())?)?,
            Registration::Uninstall { directory } => {
                confirm(cli, "Unregister the private-ai-proxy command?")?;
                value(install::uninstall(directory.clone())?)?
            }
        },
        Action::Doctor => {
            let report = doctor(&client);
            if report["errors"]
                .as_object()
                .is_some_and(|errors| !errors.is_empty())
            {
                finish_output(output(&report, cli))?;
                return Err("One or more diagnostic checks failed.".into());
            }
            report
        }
        Action::Diagnostics { output } => {
            let path = new_export_path(output)?;
            client.call(rpc::ExportDiagnostics {
                path: export_path(&path)?,
            })?;
            json!({"exported": path})
        }
        Action::App {
            command: App::Open { web },
        } => {
            // Like the desktop app, the web path fails fast during installer updates
            // instead of waiting out the startup gate.
            let data = desktop_core::paths::app_data_dir()?;
            let startup = desktop_core::lock::startup(&data)
                .map_err(|_| "Cannot acquire app startup lock")?
                .ok_or("Backend startup or an update is already in progress.")?;
            match desktop_app().filter(|_| !*web && graphical_session()) {
                Some(app) => open_desktop_app(app)?,
                None => {
                    // Backend startup takes the gate itself.
                    drop(startup);
                    open_web_ui(cli, &client)?
                }
            }
        }
        Action::Completions { .. } | Action::Schema => unreachable!(),
    };
    finish_output(output(&result, cli))
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

fn open_desktop_app(app: PathBuf) -> Result<Value, String> {
    let mut child = std::process::Command::new(app)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Cannot launch desktop UI")?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(json!({"opened": true}))
}

/// Prints a one-time login link minted over the authenticated management endpoint.
fn open_web_ui(cli: &Cli, client: &Client) -> Result<Value, String> {
    Client::ensure_service()?;
    let status = client.state()?.web_ui;
    if !status.enabled {
        if !cli.yes && (cli.json || cli.non_interactive || !io::stdin().is_terminal()) {
            return Err("The web UI is off. Enable it with `pap settings set webUi true`, or rerun with --yes.".into());
        }
        confirm(
            cli,
            &format!(
                "Web UI is off. Enable it on {}:{}?",
                desktop_core::listen::url_host(&status.listen_address),
                status.port
            ),
        )?;
        // The same change `settings set webUi true` makes.
        let mut config = client.call(rpc::Preferences)?.web_ui;
        config.enabled = true;
        client.call(rpc::SaveWebUi { config })?;
    }
    let login = client.call(rpc::WebUiLogin)?;
    let opened = graphical_session() && open_browser(&login.url).is_ok();
    Ok(json!({
        "url": login.url,
        "expiresInSeconds": login.expires_in_seconds,
        "browserOpened": opened
    }))
}

/// Open `url` in the user's browser without waiting for it.
fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "macos")]
    command.arg(url);
    #[cfg(windows)]
    let mut command = std::process::Command::new("rundll32.exe");
    #[cfg(windows)]
    command.args(["url.dll,FileProtocolHandler", url]);
    #[cfg(not(any(target_os = "macos", windows)))]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(not(any(target_os = "macos", windows)))]
    command.arg(url);
    // Without a terminal, a text-mode fallback browser cannot take over this shell.
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|_| "Cannot open a browser".to_string())?;
    // Reap the short-lived OS launcher without blocking the caller.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

fn new_export_path(path: &std::path::Path) -> Result<PathBuf, String> {
    let path = std::path::absolute(path).map_err(|_| "Cannot resolve export path")?;
    if path.exists() {
        return Err("Export target already exists; choose a new path.".into());
    }
    Ok(path)
}

fn query(filter: &UsageFilter, page: Option<&Pagination>) -> UsageQuery {
    UsageQuery {
        agent: filter.agent.clone(),
        model: filter.model.clone(),
        session_id: filter.session.clone(),
        since: filter.since,
        until: filter.until,
        cursor: page.and_then(|page| page.cursor.clone()),
        limit: page.map(|page| page.limit as usize),
    }
}

fn service_provider(provider: Provider) -> ServiceProvider {
    match provider {
        Provider::Phala => ServiceProvider::Phala,
        Provider::Redpill => ServiceProvider::Redpill,
        Provider::Custom => ServiceProvider::Custom,
    }
}

fn agent_change(
    client: &Client,
    cli: &Cli,
    id: &str,
    connect: bool,
    model: Option<String>,
    dry_run: bool,
    revision: Option<&str>,
) -> Result<Value, String> {
    let options = ConnectOptions {
        default_model: model,
    };
    if let Some(revision) = revision {
        confirm(
            cli,
            "Apply this previously previewed agent configuration revision?",
        )?;
        return value(client.call(rpc::ApplyAgent {
            agent_id: id.into(),
            connect,
            revision: revision.into(),
            options,
        })?);
    }
    let preview = client.call(rpc::PreviewAgent {
        agent_id: id.into(),
        connect,
        options: options.clone(),
    })?;
    if dry_run {
        return value(preview);
    }
    if !cli.yes && !cli.json && !cli.non_interactive && io::stdin().is_terminal() {
        desktop_core::diagnostic!("{}", output::details(&value(&preview)?));
    }
    confirm(cli, "Apply these agent configuration changes?")?;
    value(client.call(rpc::ApplyAgent {
        agent_id: id.into(),
        connect,
        revision: preview.revision,
        options,
    })?)
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
        "credentialPolicy": "OS credential store; no plaintext fallback",
        "errors": errors,
    })
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
    match result {
        Ok(value) => serde_json::to_value(value).unwrap_or_else(|_| {
            errors.insert(name.into(), json!("Cannot encode diagnostic result"));
            Value::Null
        }),
        Err(error) => {
            errors.insert(name.into(), json!(error));
            Value::Null
        }
    }
}

fn confirm(cli: &Cli, prompt: &str) -> Result<(), String> {
    if cli.yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() || cli.json || cli.non_interactive {
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
fn read_key(cli: &Cli, stdin: bool) -> Result<String, String> {
    let key = if stdin {
        if io::stdin().is_terminal() {
            return Err(
                "Refusing to read a credential from a terminal with --key-stdin; omit the flag for a hidden prompt."
                    .into(),
            );
        }
        let mut bytes = Vec::new();
        io::stdin()
            .take(514)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read credential from stdin")?;
        if bytes.len() > 513 {
            return Err("Credential input exceeds limit".into());
        }
        String::from_utf8(bytes).map_err(|_| "Credential must be UTF-8")?
    } else {
        if !io::stdin().is_terminal() || cli.json || cli.non_interactive {
            return Err("Use --key-stdin for noninteractive credential input".into());
        }
        rpassword::prompt_password("API key: ").map_err(|_| "Cannot read credential")?
    };
    agent_bridge::secrets::validate_api_key(&key)
}
fn parse_bool(value: &str) -> Result<bool, String> {
    value.parse().map_err(|_| "Expected true or false".into())
}
fn value(input: impl Serialize) -> Result<Value, String> {
    serde_json::to_value(input).map_err(|_| "Cannot encode output".into())
}

enum OutputError {
    BrokenPipe,
    Message(String),
}

fn render_output(input: &impl Serialize, cli: &Cli) -> Result<String, String> {
    let text = if cli.json {
        serde_json::to_string(input).map_err(|_| "Cannot encode output")?
    } else {
        output::render(&cli.command, &value(input)?)
    };
    Ok(text)
}

fn output(input: &impl Serialize, cli: &Cli) -> Result<(), OutputError> {
    write_text(&render_output(input, cli).map_err(OutputError::Message)?)
}

fn write_text(text: &str) -> Result<(), OutputError> {
    let mut output = io::stdout().lock();
    match writeln!(output, "{text}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(OutputError::BrokenPipe),
        Err(_) => Err(OutputError::Message("Cannot write output".into())),
    }
}

fn write_bytes(bytes: &[u8]) -> Result<(), OutputError> {
    match io::stdout().lock().write_all(bytes) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err(OutputError::BrokenPipe),
        Err(_) => Err(OutputError::Message("Cannot write output".into())),
    }
}

fn finish_output(result: Result<(), OutputError>) -> Result<(), String> {
    match result {
        Ok(()) | Err(OutputError::BrokenPipe) => Ok(()),
        Err(OutputError::Message(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn automation_modes_never_prompt_or_imply_consent() {
        for mode in ["--json", "--non-interactive", "--no-interactive"] {
            let cli = Cli::try_parse_from(["private-ai-proxy", mode, "status"]).unwrap();
            assert!(confirm(&cli, "Confirm?").unwrap_err().contains("--yes"));
            assert!(read_key(&cli, false).unwrap_err().contains("--key-stdin"));
            let approved =
                Cli::try_parse_from(["private-ai-proxy", mode, "--yes", "status"]).unwrap();
            assert!(confirm(&approved, "Confirm?").is_ok());
        }
    }

    #[test]
    fn human_output_is_a_summary_not_a_state_dump() {
        let state = json!({"gateway": {"status": "verified", "activeProfileId": "work", "proxyUrl": "http://127.0.0.1:4180", "activity": [{"detail": "not part of status"}]}});
        assert_eq!(
            output::render(&Action::Status { watch: false }, &state),
            "Protected\nProfile: work\nLocal API: http://127.0.0.1:4180"
        );
        assert_eq!(
            output::render(
                &Action::Profiles {
                    command: Profiles::List
                },
                &json!([])
            ),
            "No profiles."
        );
        assert_eq!(
            output::render(
                &Action::Token {
                    command: Token::Show
                },
                &json!({"token":"sk-pap-example"})
            ),
            "sk-pap-example"
        );
        let agents = output::render(
            &Action::Agents {
                command: Agents::List,
            },
            &json!([
                {"id":"codex", "name":"Codex", "installed":true, "connected":false}
            ]),
        );
        assert_eq!(agents, "codex  Codex  Not connected");
    }
}
