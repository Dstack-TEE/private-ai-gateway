//! `aci` — reference client for the ACI protocol (`spec/aci.md`).
//!
//! Verify a live service, audit saved artifacts offline, or run one
//! verified chat completion end to end.

#[path = "../../../apps/desktop/cli/args.rs"]
mod args;
#[path = "../../../apps/desktop/cli/audit.rs"]
mod audit;
#[path = "../../../apps/desktop/cli/capture.rs"]
mod capture;
#[path = "../../../apps/desktop/cli/checks.rs"]
mod checks;
#[path = "../../../apps/desktop/cli/client.rs"]
mod client;
#[path = "../../../apps/desktop/cli/send.rs"]
mod send;
#[path = "../../../apps/desktop/cli/serve.rs"]
mod serve;
#[path = "../../../apps/desktop/cli/sessions.rs"]
mod sessions;
#[cfg(test)]
#[path = "../../../apps/desktop/cli/spec_fixtures.rs"]
mod spec_fixtures;
#[path = "../../../apps/desktop/cli/transcript.rs"]
mod transcript;
#[path = "../../../apps/desktop/cli/verify.rs"]
mod verify;

use clap::Parser;

#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err}");
            1
        }
    };
    std::process::exit(code);
}

async fn run() -> Result<i32, String> {
    let args = args::Cli::parse();
    match args.command {
        args::Command::Verify(a) => verify::run(a, args.require_production_os).await,
        args::Command::Audit(a) => audit::run(a, args.require_production_os).await,
        args::Command::Sessions(a) => sessions::run(a, args.require_production_os).await,
        args::Command::Send(a) => send::run(a, args.require_production_os).await,
        args::Command::Serve(a) => serve::run(a, args.require_production_os).await,
    }
}
