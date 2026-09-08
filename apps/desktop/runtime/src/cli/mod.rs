use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::{
    client::Client,
    contracts::*,
    preferences::{Appearance, UpdateChannel},
    protocol::{Command, Preference},
    usage::UsageQuery,
};
use clap::{CommandFactory, FromArgMatches, Parser};
use serde::Serialize;
use serde_json::{json, Map, Value};

mod args;
#[path = "output.rs"]
mod human;
mod schema;

use args::*;

pub fn cli_command() -> clap::Command {
    Cli::command()
}

pub fn run_matches(matches: &clap::ArgMatches, command: clap::Command) -> Result<(), String> {
    let cli = Cli::from_arg_matches(matches).map_err(|error| error.to_string())?;
    execute(&cli, command)
}

pub fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let json_requested = args
        .iter()
        .skip(1)
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(error) if json_requested && error.use_stderr() => {
            eprintln!(
                "{}",
                json!({"error": {"code": "invalid_arguments", "message": error.to_string()}})
            );
            std::process::exit(error.exit_code());
        }
        Err(error) => error.exit(),
    };
    if let Err(error) = execute(&cli, Cli::command()) {
        if cli.json {
            eprintln!(
                "{}",
                json!({"error": {"code": "command_failed", "message": error}})
            );
        } else {
            eprintln!("pag: {}", human::safe(&error));
        }
        std::process::exit(1);
    }
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
                    client.activate_profile(profile.clone())?;
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
                    client.start(state.config)?
                };
                let deadline = Instant::now() + Duration::from_secs(*timeout);
                loop {
                    let state = client.state()?;
                    if state.session_id != started.session_id {
                        return Err(
                            "The gateway operation was superseded by another client.".into()
                        );
                    }
                    match state.status.as_str() {
                        "verified" if !state.configuration_verification => break value(state)?,
                        "verifying" => {}
                        _ => {
                            return Err(
                                "Gateway did not become verified. Inspect pag status.".into()
                            )
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err("Verification wait timed out; the backend may still be verifying. Inspect pag status before retrying.".into());
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
        Action::Stop => value(client.stop()?)?,
        Action::Profiles { command } => {
            match command {
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
                    let backup = crate::maintenance::ProfileBackup::read(file)?;
                    confirm(
                        cli,
                        "Import these unverified profile configurations without credentials?",
                    )?;
                    Client::ensure_service()?;
                    value(client.import_profiles(backup)?)?
                }
                Profiles::Export { output } => {
                    let path = new_export_path(output)?;
                    client.export_profiles(path.clone())?;
                    json!({"exported": path})
                }
                Profiles::Use { id } => {
                    confirm(cli, "Switch the active profile?")?;
                    value(client.activate_profile(id.clone())?)?
                }
                Profiles::Remove { id } => {
                    confirm(cli, "Delete this profile and its stored credential?")?;
                    value(client.delete_profile(id.clone())?)?
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
                        provider: match provider {
                            Provider::Phala => ServiceProvider::Phala,
                            Provider::Redpill => ServiceProvider::Redpill,
                            Provider::Custom => ServiceProvider::Custom,
                        },
                    };
                    // Validate before reading a credential or making a request.
                    crate::service_config::resolve_profile(profile.clone(), None)?;
                    confirm(
                        cli,
                        "Verify and save this profile? This selects it as active and may restart protection.",
                    )?;
                    Client::ensure_service()?;
                    if client.state()?.profiles.iter().any(|saved| saved.id == *id) {
                        return Err("Profile ID already exists. Use profiles verify to update its credential.".into());
                    }
                    let key = read_key(cli, *key_stdin)?;
                    client.request(Command::Verify {
                        profile,
                        require_production_os: !allow_development_os,
                        key: Some(key),
                    })?
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
                        "Verify and save this profile? This selects it as active and may restart protection.",
                    )?;
                    let key = if *key_stdin
                        || !profile
                            .credential_saved
                            .unwrap_or(profile.verified_at.is_some())
                    {
                        Some(read_key(cli, *key_stdin)?)
                    } else {
                        None
                    };
                    client.request(Command::Verify {
                        profile: ConfidentialProfileInput {
                            id: profile.id,
                            name: profile.name,
                            provider: profile.provider,
                            remote_url: profile.remote_url,
                        },
                        require_production_os: state.config.require_production_os,
                        key,
                    })?
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
                        provider: provider
                            .map(service_provider)
                            .unwrap_or_else(|| saved.provider.clone()),
                        remote_url: url.clone().unwrap_or_else(|| saved.remote_url.clone()),
                    };
                    let resolved = crate::service_config::resolve_profile(profile.clone(), None)?;
                    let target_changed = resolved.provider != saved.provider
                        || resolved.remote_url != saved.remote_url;
                    let credential_saved = saved
                        .credential_saved
                        .unwrap_or(saved.verified_at.is_some());
                    confirm(
                        cli,
                        "Verify and save these profile changes? This selects the profile as active and may restart protection.",
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
                    client.request(Command::Verify {
                        profile,
                        require_production_os: production_os,
                        key,
                    })?
                }
            }
        }
        Action::Agents { command } => match command {
            Agents::List => value(client.list_agents()?)?,
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
                value(client.disconnect_all_agents()?)?
            }
        },
        Action::Models {
            command: Models::List { refresh },
        } => {
            let state: GatewayState = if *refresh {
                client.request(Command::RefreshCatalog)?
            } else {
                client.state()?
            };
            value(
                state
                    .catalog
                    .ok_or("No verified model catalog. Start and verify the gateway first.")?,
            )?
        }
        Action::Usage { command } => match command {
            Usage::List { filter, page } => value(client.query_usage(query(filter, Some(page)))?)?,
            Usage::Show { id } => value(client.usage_record(id)?.ok_or("Usage record not found")?)?,
            Usage::Export { filter, output, .. } => {
                let path = std::path::absolute(output).map_err(|_| "Cannot resolve export path")?;
                if path.exists() {
                    return Err("Export target already exists; choose a new path.".into());
                }
                value(json!({"rows": client.export_usage_csv(query(filter, None), path)?}))?
            }
            Usage::Clear => {
                confirm(cli, "Permanently clear all usage history?")?;
                json!({"deleted": client.clear_usage()?})
            }
        },
        Action::Settings { command } => {
            match command {
                Settings::Reset => {
                    confirm(cli, "Stop protection, restore all agents, and reset backend settings? Profiles, keys and usage are kept. Open at Login is managed by the desktop app.")?;
                    value(client.reset_settings()?)?
                }
                Settings::Show => {
                    json!({"preferences": client.preferences()?, "localApi": client.state()?.local_api})
                }
                Settings::Set { key, value: input } => {
                    confirm(cli, "Change gateway settings?")?;
                    match key {
                    SettingsKey::AutoCliRegistration => value(
                        client.set_preference(Preference::AutoCliRegistration(parse_bool(input)?))?,
                    )?,
                    SettingsKey::Notifications => value(client.set_preference(Preference::Notifications(
                        serde_json::from_str(input).map_err(|_| "Expected notification settings as a JSON object with boolean values")?
                    ))?)?,
                    SettingsKey::ConnectOnLaunch => value(
                        client.set_preference(Preference::ConnectOnLaunch(parse_bool(input)?))?,
                    )?,
                    SettingsKey::Appearance => value(client.set_preference(Preference::Appearance(
                        match input.as_str() {
                            "system" => Appearance::System,
                            "light" => Appearance::Light,
                            "dark" => Appearance::Dark,
                            _ => return Err("Expected system, light, or dark".into()),
                        },
                    ))?)?,
                    SettingsKey::UpdateChannel => value(client.set_preference(Preference::UpdateChannel(
                        match input.as_str() {
                            "beta" => UpdateChannel::Beta,
                            "stable" => UpdateChannel::Stable,
                            _ => return Err("Expected beta or stable".into()),
                        },
                    ))?)?,
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
                        crate::local_api::resolve(config.clone())?;
                        client.request(Command::SaveLocalApi(config))?
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
                let _ = client.rotate_client_key()?;
                json!({"rotated": true})
            }
            Token::Show => {
                confirm(cli, "Reveal the local API token on stdout?")?;
                json!({"token": client.client_key()?})
            }
            Token::ClearCredential => {
                confirm(cli, "Remove the active profile credential?")?;
                value(client.clear_api_key()?)?
            }
        },
        Action::Cli { command } => match command {
            Registration::Status => value(crate::cli_install::status()?)?,
            Registration::Install { directory } => {
                value(crate::cli_install::install(directory.clone())?)?
            }
            Registration::Uninstall { directory } => {
                confirm(cli, "Unregister the pag command?")?;
                value(crate::cli_install::uninstall(directory.clone())?)?
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
            client.export_diagnostics(path.clone())?;
            json!({"exported": path})
        }
        Action::App { command: App::Open } => {
            let data = desktop_gateway::agents::app_data_dir()?;
            let _startup = desktop_gateway::lock::startup(&data)
                .map_err(|_| "Cannot acquire app startup lock")?
                .ok_or("Backend startup or an update is already in progress.")?;
            let backend = crate::launch::service_executable()?;
            let app =
                backend
                    .parent()
                    .ok_or("Cannot locate app directory")?
                    .join(if cfg!(windows) {
                        "private-ai-gateway-desktop.exe"
                    } else {
                        "private-ai-gateway-desktop"
                    });
            if !app.is_file() {
                return Err("Desktop UI is not installed alongside this CLI.".into());
            }
            let mut child = std::process::Command::new(app)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|_| "Cannot launch desktop UI")?;
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            json!({"opened": true})
        }
        Action::Completions { .. } | Action::Schema => unreachable!(),
    };
    finish_output(output(&result, cli))
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
        return value(client.apply_agent(id.into(), connect, revision.into(), options)?);
    }
    let preview = client.preview_agent(id.into(), connect, options.clone())?;
    if dry_run {
        return value(preview);
    }
    if !cli.yes && !cli.json && !cli.non_interactive && io::stdin().is_terminal() {
        eprintln!("{}", human::details(&value(&preview)?));
    }
    confirm(cli, "Apply these agent configuration changes?")?;
    value(client.apply_agent(id.into(), connect, preview.revision, options)?)
}

fn doctor(client: &Client) -> Value {
    let mut errors = Map::new();
    let backend_running = doctor_check(&mut errors, "backendRunning", client.is_running());
    let cli = doctor_check(&mut errors, "cli", crate::cli_install::status());
    let backend_executable = doctor_check(
        &mut errors,
        "backendExecutable",
        crate::launch::service_executable(),
    );
    let endpoint = doctor_check(
        &mut errors,
        "endpoint",
        crate::transport::endpoint_path()
            .map_err(|_| "Cannot resolve management endpoint".to_string()),
    );
    json!({
        "version": crate::protocol::BUILD_VERSION,
        "backendRunning": backend_running,
        "cli": cli,
        "backendExecutable": backend_executable,
        "endpoint": endpoint,
        "credentialPolicy": "OS credential store; no plaintext fallback",
        "errors": errors,
    })
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
    desktop_gateway::secrets::validate_api_key(&key)
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
        human::render(&cli.command, &value(input)?)
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

    #[test]
    fn automation_modes_never_prompt_or_imply_consent() {
        for mode in ["--json", "--non-interactive", "--no-interactive"] {
            let cli = Cli::try_parse_from(["pag", mode, "status"]).unwrap();
            assert!(confirm(&cli, "Confirm?").unwrap_err().contains("--yes"));
            assert!(read_key(&cli, false).unwrap_err().contains("--key-stdin"));
            let approved = Cli::try_parse_from(["pag", mode, "--yes", "status"]).unwrap();
            assert!(confirm(&approved, "Confirm?").is_ok());
        }
    }

    #[test]
    fn human_output_is_a_summary_not_a_state_dump() {
        let state = json!({"gateway": {"status": "verified", "activeProfileId": "work", "proxyUrl": "http://127.0.0.1:4180", "activity": [{"detail": "not part of status"}]}});
        assert_eq!(
            human::render(&Action::Status { watch: false }, &state),
            "Protected\nProfile: work\nLocal API: http://127.0.0.1:4180"
        );
        assert_eq!(
            human::render(
                &Action::Profiles {
                    command: Profiles::List
                },
                &json!([])
            ),
            "No profiles."
        );
        assert_eq!(
            human::render(
                &Action::Token {
                    command: Token::Show
                },
                &json!({"token":"sk-pag-example"})
            ),
            "sk-pag-example"
        );
        let agents = human::render(
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
