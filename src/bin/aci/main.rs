//! `aci` — reference client for the ACI protocol (`spec/aci.md`).
//!
//! Verify a live service, audit saved artifacts offline, or run one
//! verified chat completion end to end.

#[path = "../private-ai-proxy/args.rs"]
mod args;
#[path = "../private-ai-proxy/audit.rs"]
mod audit;
#[path = "../private-ai-proxy/capture.rs"]
mod capture;
#[path = "../private-ai-proxy/checks.rs"]
mod checks;
#[path = "../private-ai-proxy/client.rs"]
mod client;
#[path = "../private-ai-proxy/send.rs"]
mod send;
#[path = "../private-ai-proxy/serve.rs"]
mod serve;
#[path = "../private-ai-proxy/sessions.rs"]
mod sessions;
#[cfg(test)]
#[path = "../private-ai-proxy/spec_fixtures.rs"]
mod spec_fixtures;
#[path = "../private-ai-proxy/transcript.rs"]
mod transcript;
#[path = "../private-ai-proxy/verify.rs"]
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
