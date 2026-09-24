//! Unified Private AI Proxy CLI, including the ACI protocol commands.
mod manage;

use clap::{FromArgMatches, Subcommand};
use private_ai_proxy::{args, audit, send, serve, sessions, verify};
use std::{ffi::OsStr, io::IsTerminal, path::Path};

#[tokio::main]
async fn main() {
    desktop_core::logging::init();
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
        // Management calls block on the local API; keep them off the async workers.
        Err(_) => tokio::task::spawn_blocking(move || manage::run_matches(&matches, command))
            .await
            .unwrap_or_else(|_| Err("The command could not complete".into()))
            .map(|()| 0),
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
    let machine_output = std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| arg == "--json" || arg == "--json-events");
    if invoked_as_aci(
        std::env::args_os().next().as_deref(),
        std::env::var_os(desktop_core::launch::ALIAS_ENV).as_deref(),
    ) && !machine_output
        && std::io::stderr().is_terminal()
    {
        tracing::info!("note: `aci` is a legacy alias; use `pap` or `private-ai-proxy` instead.");
    }
}

/// Symlinks and the npm launcher name the alias in `argv[0]`; Windows `.cmd`
/// shims, which cannot, name it in [`desktop_core::launch::ALIAS_ENV`].
fn invoked_as_aci(argv0: Option<&OsStr>, alias: Option<&OsStr>) -> bool {
    alias == Some(OsStr::new("aci"))
        || argv0.is_some_and(|name| Path::new(name).file_stem() == Some(OsStr::new("aci")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_alias_channel_is_recognized() {
        let name = |value: &'static str| Some(OsStr::new(value));
        // Linux packages and `pap cli install` symlinks, and the npm launcher's argv0.
        assert!(invoked_as_aci(name("/usr/bin/aci"), None));
        assert!(invoked_as_aci(name("aci"), None));
        // A Windows `.cmd` shim runs the executable by its own path.
        assert!(invoked_as_aci(
            name(r"C:\pap\private-ai-proxy.exe"),
            name("aci")
        ));
        assert!(!invoked_as_aci(name("/usr/bin/pap"), None));
        assert!(!invoked_as_aci(name("private-ai-proxy"), name("pap")));
        assert!(!invoked_as_aci(None, None));
    }
}
