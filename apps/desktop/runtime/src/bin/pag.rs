use std::{
    io::{self, IsTerminal, Read, Write},
    path::PathBuf,
    time::{Duration, Instant},
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use desktop_runtime::{
    client::Client,
    contracts::*,
    preferences::{Appearance, UpdateChannel},
    protocol::{Command, Preference},
    usage::UsageQuery,
};
use serde::Serialize;
use serde_json::{json, Value};

#[path = "pag/output.rs"]
mod human;

#[derive(Parser)]
#[command(name = "pag", version = desktop_runtime::protocol::BUILD_VERSION, about = "Control the Private AI Gateway backend")]
struct Cli {
    #[arg(long, global = true)]
    json: bool,
    #[arg(
        long,
        visible_alias = "no-interactive",
        global = true,
        help = "Never prompt; use --yes to approve changes and --key-stdin for credentials"
    )]
    non_interactive: bool,
    #[arg(
        long,
        global = true,
        help = "Confirm configuration changes without prompting"
    )]
    yes: bool,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    Status {
        #[arg(long)]
        watch: bool,
    },
    Start {
        #[arg(long)]
        profile: Option<String>,
        #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    Stop,
    Service {
        #[command(subcommand)]
        command: Service,
    },
    Profiles {
        #[command(subcommand)]
        command: Profiles,
    },
    Agents {
        #[command(subcommand)]
        command: Agents,
    },
    Models {
        #[command(subcommand)]
        command: Models,
    },
    Usage {
        #[command(subcommand)]
        command: Usage,
    },
    Settings {
        #[command(subcommand)]
        command: Settings,
    },
    Token {
        #[command(subcommand)]
        command: Token,
    },
    Cli {
        #[command(subcommand)]
        command: Registration,
    },
    App {
        #[command(subcommand)]
        command: App,
    },
    Doctor,
    Diagnostics {
        #[arg(long)]
        output: PathBuf,
    },
}

#[derive(Subcommand)]
enum Service {
    Start,
    Stop,
    Status,
}
#[derive(Subcommand)]
enum App {
    Open,
}
#[derive(Subcommand)]
enum Registration {
    Status,
    Install {
        #[arg(long)]
        directory: Option<PathBuf>,
    },
    Uninstall {
        #[arg(long)]
        directory: Option<PathBuf>,
    },
}
#[derive(Subcommand)]
enum Token {
    Rotate,
    Show,
    ClearCredential,
}
#[derive(Subcommand)]
enum Models {
    List {
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Provider {
    Phala,
    Redpill,
    Custom,
}
#[derive(Subcommand)]
enum Profiles {
    List,
    Import {
        file: PathBuf,
    },
    Export {
        #[arg(long)]
        output: PathBuf,
    },
    Add {
        #[arg(long)]
        id: String,
        #[arg(long)]
        name: String,
        #[arg(long)]
        url: String,
        #[arg(long, value_enum, default_value_t = Provider::Custom)]
        provider: Provider,
        #[arg(long)]
        key_stdin: bool,
        #[arg(long)]
        allow_development_os: bool,
    },
    Verify {
        id: String,
        #[arg(long)]
        key_stdin: bool,
    },
    Use {
        id: String,
    },
    Remove {
        id: String,
    },
}

#[derive(Subcommand)]
enum Agents {
    List,
    Connect {
        id: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        dry_run: bool,
    },
    Disconnect {
        id: String,
        #[arg(long)]
        dry_run: bool,
    },
    DisconnectAll,
}

#[derive(Args)]
struct Filter {
    #[arg(long)]
    agent: Option<String>,
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    since: Option<u64>,
    #[arg(long)]
    until: Option<u64>,
    #[arg(long)]
    cursor: Option<String>,
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=100))]
    limit: u64,
}
impl From<Filter> for UsageQuery {
    fn from(filter: Filter) -> Self {
        Self {
            agent: filter.agent,
            model: filter.model,
            session_id: filter.session,
            since: filter.since,
            until: filter.until,
            cursor: filter.cursor,
            limit: Some(filter.limit as usize),
        }
    }
}
#[derive(Subcommand)]
enum Usage {
    List {
        #[command(flatten)]
        filter: Filter,
    },
    Show {
        id: String,
    },
    Export {
        #[command(flatten)]
        filter: Filter,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "csv", value_parser = ["csv"])]
        format: String,
    },
    Clear,
}

#[derive(Subcommand)]
enum Settings {
    Show,
    Set { key: String, value: String },
}

fn main() {
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
    if let Err(error) = execute(&cli) {
        if error == "Output pipe closed" {
            return;
        }
        if cli.json {
            eprintln!(
                "{}",
                json!({"error": {"code": "command_failed", "message": error}})
            );
        } else {
            eprintln!("pag: {error}");
        }
        std::process::exit(1);
    }
}

fn execute(cli: &Cli) -> Result<(), String> {
    let client = Client::new();
    let result = match &cli.command {
        Action::Status { watch: true } => {
            return Client::watch_connection(|state| output(&state, cli).is_ok());
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
                Profiles::Import { file } => {
                    let backup = desktop_runtime::maintenance::ProfileBackup::read(file)?;
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
                    desktop_runtime::service_config::resolve_profile(profile.clone(), None)?;
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
            }
        }
        Action::Agents { command } => match command {
            Agents::List => value(client.list_agents()?)?,
            Agents::Connect { id, model, dry_run } => {
                agent_change(&client, cli, id, true, model.clone(), *dry_run)?
            }
            Agents::Disconnect { id, dry_run } => {
                agent_change(&client, cli, id, false, None, *dry_run)?
            }
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
            Usage::List { filter } => value(client.query_usage(query(filter))?)?,
            Usage::Show { id } => value(client.usage_record(id)?.ok_or("Usage record not found")?)?,
            Usage::Export { filter, output, .. } => {
                let path = std::path::absolute(output).map_err(|_| "Cannot resolve export path")?;
                if path.exists() {
                    return Err("Export target already exists; choose a new path.".into());
                }
                value(json!({"rows": client.export_usage_csv(query(filter), path)?}))?
            }
            Usage::Clear => {
                confirm(cli, "Permanently clear all usage history?")?;
                json!({"deleted": client.clear_usage()?})
            }
        },
        Action::Settings { command } => {
            match command {
                Settings::Show => {
                    json!({"preferences": client.preferences()?, "localApi": client.state()?.local_api})
                }
                Settings::Set { key, value: input } => {
                    confirm(cli, "Change gateway settings?")?;
                    match key.as_str() {
                    "autoCliRegistration" => value(
                        client.set_preference(Preference::AutoCliRegistration(parse_bool(input)?))?,
                    )?,
                    "notifications" => value(client.set_preference(Preference::Notifications(
                        serde_json::from_str(input).map_err(|_| "Expected notification settings as a JSON object with boolean values")?
                    ))?)?,
                    "connectOnLaunch" => value(
                        client.set_preference(Preference::ConnectOnLaunch(parse_bool(input)?))?,
                    )?,
                    "appearance" => value(client.set_preference(Preference::Appearance(
                        match input.as_str() {
                            "system" => Appearance::System,
                            "light" => Appearance::Light,
                            "dark" => Appearance::Dark,
                            _ => return Err("Expected system, light, or dark".into()),
                        },
                    ))?)?,
                    "updateChannel" => value(client.set_preference(Preference::UpdateChannel(
                        match input.as_str() {
                            "beta" => UpdateChannel::Beta,
                            "stable" => UpdateChannel::Stable,
                            _ => return Err("Expected beta or stable".into()),
                        },
                    ))?)?,
                    _ => {
                        let mut config = client.state()?.local_api;
                        match key.as_str() {
                            "listenAddress" => config.listen_address = input.clone(),
                            "allowNetworkAccess" => {
                                config.allow_network_access = parse_bool(input)?
                            }
                            "port" => {
                                config.port =
                                    input.parse().map_err(|_| "Expected a valid port number")?
                            }
                            "clientHost" => {
                                config.client_host = (!input.is_empty()).then(|| input.clone())
                            }
                            _ => return Err("Unknown setting".into()),
                        }
                        desktop_runtime::local_api::resolve(config.clone())?;
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
            Registration::Status => value(desktop_runtime::cli_install::status()?)?,
            Registration::Install { directory } => {
                value(desktop_runtime::cli_install::install(directory.clone())?)?
            }
            Registration::Uninstall { directory } => {
                confirm(cli, "Unregister the pag command?")?;
                value(desktop_runtime::cli_install::uninstall(directory.clone())?)?
            }
        },
        Action::Doctor => {
            json!({"version": desktop_runtime::protocol::BUILD_VERSION, "backendRunning": client.is_running()?, "cli": desktop_runtime::cli_install::status()?, "backendExecutable": desktop_runtime::launch::service_executable()?, "endpoint": desktop_runtime::transport::endpoint_path().map_err(|_| "Cannot resolve management endpoint")?, "credentialPolicy": "OS credential store; no plaintext fallback"})
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
            let backend = desktop_runtime::launch::service_executable()?;
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
    };
    output(&result, cli)
}

fn new_export_path(path: &std::path::Path) -> Result<PathBuf, String> {
    let path = std::path::absolute(path).map_err(|_| "Cannot resolve export path")?;
    if path.exists() {
        return Err("Export target already exists; choose a new path.".into());
    }
    Ok(path)
}

fn query(filter: &Filter) -> UsageQuery {
    UsageQuery {
        agent: filter.agent.clone(),
        model: filter.model.clone(),
        session_id: filter.session.clone(),
        since: filter.since,
        until: filter.until,
        cursor: filter.cursor.clone(),
        limit: Some(filter.limit as usize),
    }
}
fn agent_change(
    client: &Client,
    cli: &Cli,
    id: &str,
    connect: bool,
    model: Option<String>,
    dry_run: bool,
) -> Result<Value, String> {
    let options = ConnectOptions {
        default_model: model,
    };
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
fn output(input: &impl Serialize, cli: &Cli) -> Result<(), String> {
    let text = if cli.json {
        serde_json::to_string(input).map_err(|_| "Cannot encode output")?
    } else {
        human::render(&cli.command, &value(input)?)
    };
    match writeln!(io::stdout().lock(), "{text}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Err("Output pipe closed".into()),
        Err(_) => Err("Cannot write output".into()),
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
