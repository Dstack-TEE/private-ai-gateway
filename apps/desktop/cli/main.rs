//! Unified Private AI Proxy CLI, including the ACI protocol commands.
mod manage;

use clap::{FromArgMatches, Subcommand};
use private_ai_proxy::{args, audit, send, serve, sessions, verify};
use std::{io::IsTerminal, path::Path};

#[tokio::main]
async fn main() {
    private_ai_proxy::install_crypto_provider();
    let command = args::Command::augment_subcommands(manage::cli_command())
        .name("private-ai-proxy")
        .about("Private AI Proxy: manage local protection and verify confidential AI services")
        .long_about("Manage profiles, coding agents and local protection, or verify and audit ACI services without starting the managed backend.")
        .arg(clap::Arg::new("require_production_os").long("require-production-os").help("Require an attested production OS image").global(true).action(clap::ArgAction::SetTrue));
    let json = std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json");
    legacy_alias_hint();
    let matches = command.clone().try_get_matches().unwrap_or_else(|error| {
        if json && error.use_stderr() {
            eprintln!("{}", serde_json::json!({"error":{"code":"invalid_arguments","message":error.to_string()}}));
            std::process::exit(error.exit_code());
        }
        error.exit()
    });
    let json = matches.get_flag("json");
    let result = match args::Command::from_arg_matches(&matches) {
        Ok(command) => {
            let production = matches.get_flag("require_production_os");
            match command {
                args::Command::Verify(a) => verify::run(a, production).await,
                args::Command::Audit(a) => audit::run(a, production).await,
                args::Command::Sessions(a) => sessions::run(a, production).await,
                args::Command::Send(a) => send::run(a, production).await,
                args::Command::Serve(mut a) => {
                    a.json_events |= json;
                    serve::run(a, production).await
                }
            }
        }
        Err(_) => manage::run_matches(&matches, command).map(|()| 0),
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

/// `aci` is a legacy alias of this executable. Interactive use gets a one-line
/// nudge toward `pap`; machine-readable modes and redirected stderr never do.
fn legacy_alias_hint() {
    let invoked_as_aci = std::env::args_os().next().is_some_and(|name| {
        Path::new(&name)
            .file_stem()
            .is_some_and(|stem| stem == "aci")
    });
    let machine_output = std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json" || arg == "--json-events");
    if invoked_as_aci && !machine_output && std::io::stderr().is_terminal() {
        desktop_core::diagnostic!(
            "note: `aci` is a legacy alias; use `pap` or `private-ai-proxy` instead."
        );
    }
}
