//! Unified Private AI Proxy CLI, including the ACI protocol commands.
mod args;
mod audit;
mod capture;
mod checks;
mod client;
use desktop_runtime::cli as managed;
mod send;
mod serve;
mod sessions;
#[cfg(test)]
mod spec_fixtures;
mod transcript;
mod verify;

use clap::{FromArgMatches, Subcommand};

#[tokio::main]
async fn main() {
    let command = args::Command::augment_subcommands(managed::cli_command())
        .name("private-ai-proxy")
        .about("Private AI Proxy: manage local protection and verify confidential AI services")
        .long_about("Manage profiles, coding agents and local protection, or verify and audit ACI services without starting the managed backend.")
        .mut_subcommand("cli", |command| command.about("Manage installation of the private-ai-proxy command"))
        .mut_subcommand("serve", |command| command.about("Run a local verifying proxy; stream responses and audit receipts after delivery"))
        .arg(clap::Arg::new("require_production_os").long("require-production-os").help("Require an attested production OS image").global(true).action(clap::ArgAction::SetTrue));
    let json = std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json");
    let matches = command.clone().try_get_matches().unwrap_or_else(|error| {
        if json && error.use_stderr() {
            eprintln!("{}", serde_json::json!({"error":{"code":"invalid_arguments","message":error.to_string()}}));
            std::process::exit(error.exit_code());
        }
        error.exit()
    });
    let result = match matches.subcommand_name() {
        Some("verify" | "audit" | "sessions" | "send" | "serve") => {
            let production = matches.get_flag("require_production_os");
            match args::Command::from_arg_matches(&matches) {
                Ok(args::Command::Verify(a)) => verify::run(a, production).await,
                Ok(args::Command::Audit(a)) => audit::run(a, production).await,
                Ok(args::Command::Sessions(a)) => sessions::run(a, production).await,
                Ok(args::Command::Send(a)) => send::run(a, production).await,
                Ok(args::Command::Serve(mut a)) => {
                    a.audit_receipts = true;
                    a.json_events |= json;
                    serve::run(a, production).await
                }
                Err(error) => Err(error.to_string()),
            }
        }
        _ => managed::run_matches(&matches, command).map(|()| 0),
    };
    let code = match result {
        Ok(code) => code,
        Err(error) => {
            if json {
                eprintln!(
                    "{}",
                    serde_json::json!({"error":{"code":"command_failed","message":error}})
                );
            } else {
                eprintln!("private-ai-proxy: {error}");
            }
            1
        }
    };
    std::process::exit(code);
}
