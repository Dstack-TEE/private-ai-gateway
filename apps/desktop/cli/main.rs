//! Unified Private AI Proxy CLI, including the ACI protocol commands.
mod manage;

use clap::{Args, Parser, Subcommand};
use desktop_core::client::CallError;
use private_ai_proxy::{args::Command as Aci, audit, curl, send, serve, sessions, verify};
use std::{ffi::OsStr, io::IsTerminal, path::Path};

#[derive(Parser)]
#[command(
    name = "private-ai-proxy",
    version = desktop_core::protocol::BUILD_VERSION,
    about = "Private AI Proxy: manage local protection and verify confidential AI services",
    long_about = "Manage profiles, coding agents and local protection, or verify and audit ACI services without starting the managed backend."
)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Command,
    /// Require an attested production OS image
    // Declared after the commands, so help lists it after their own options.
    #[arg(long, global = true)]
    require_production_os: bool,
}

/// Options every command accepts.
#[derive(Args)]
struct Global {
    /// Emit compact JSON instead of human-readable output.
    #[arg(long, global = true)]
    json: bool,
    /// Never prompt. Mutations require --yes; credential inputs use stdin flags.
    #[arg(long, visible_alias = "no-interactive", global = true)]
    non_interactive: bool,
    /// Approve a command's documented mutation without prompting.
    #[arg(long, global = true)]
    yes: bool,
}

impl Global {
    /// Whether the user can be prompted: a terminal, and no automation flag.
    fn interactive(&self) -> bool {
        !self.json && !self.non_interactive && std::io::stdin().is_terminal()
    }
}

#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Manage(manage::Action),
    #[command(flatten)]
    Aci(Aci),
}

#[tokio::main]
async fn main() {
    desktop_core::logging::init();
    private_ai_proxy::install_crypto_provider();
    legacy_alias_hint();
    let Cli {
        global,
        command,
        require_production_os: production,
    } = Cli::try_parse().unwrap_or_else(|error| {
        // Parsing failed, so `--json` is known only from the raw arguments.
        if machine_output(&["--json"]) && error.use_stderr() {
            eprintln!("{}", serde_json::json!({"error":{"code":"invalid_arguments","message":error.to_string()}}));
            std::process::exit(error.exit_code());
        }
        error.exit()
    });
    let json = global.json;
    let aci = |result: Result<i32, String>| result.map_err(CallError::Local);
    let (result, failure_code) = match command {
        Command::Aci(Aci::Verify(a)) => (aci(verify::run(a, production).await), 1),
        Command::Aci(Aci::Audit(a)) => (aci(audit::run(a, production).await), 1),
        Command::Aci(Aci::Sessions(a)) => (aci(sessions::run(a, production).await), 1),
        Command::Aci(Aci::Send(a)) => (aci(send::run(a, production).await), 1),
        // Keep pap's own failures apart from curl's exit codes.
        Command::Aci(Aci::Curl(a)) => (
            aci(curl::run(a, production).await),
            curl::PAP_FAILURE_EXIT_CODE,
        ),
        Command::Aci(Aci::Serve(mut a)) => {
            a.json_events |= json;
            (aci(serve::run(a, production).await), 1)
        }
        // Management calls block on the local API; keep them off the async workers.
        Command::Manage(action) => {
            let result = tokio::task::spawn_blocking(move || manage::run(&global, &action))
                .await
                .unwrap_or_else(|_| Err("The command could not complete".into()))
                .map(|()| 0);
            (result, 1)
        }
    };
    let code = result.unwrap_or_else(|error| {
        if json {
            eprintln!("{}", json_error(error));
        } else {
            eprintln!("private-ai-proxy: {error}");
        }
        failure_code
    });
    std::process::exit(code);
}

/// Whether any of `flags` precedes a `--` in the raw arguments.
fn machine_output(flags: &[&str]) -> bool {
    std::env::args_os()
        .skip(1)
        .take_while(|arg| arg != "--")
        .any(|arg| flags.iter().any(|flag| arg == *flag))
}

/// A failure as `--json` reports it: the backend's error code, or
/// `command_failed` for one reported by the CLI itself.
fn json_error(error: CallError) -> serde_json::Value {
    let (code, message) = match error {
        CallError::Api(error) => (serde_json::json!(error.code), error.message),
        CallError::Local(message) => (serde_json::json!("command_failed"), message),
    };
    serde_json::json!({ "error": { "code": code, "message": message } })
}

/// `aci` is a legacy alias of this executable. Interactive use gets a one-line
/// nudge toward `pap`; machine-readable modes and redirected stderr never do.
fn legacy_alias_hint() {
    if invoked_as_aci(
        std::env::args_os().next().as_deref(),
        std::env::var_os(desktop_core::launch::ALIAS_ENV).as_deref(),
    ) && !machine_output(&["--json", "--json-events"])
        && std::io::stderr().is_terminal()
    {
        eprintln!("note: `aci` is a legacy alias; use `pap` or `private-ai-proxy` instead.");
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
