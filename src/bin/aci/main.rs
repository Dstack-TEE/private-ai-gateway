//! `aci` — reference client for the ACI protocol (`spec/aci.md`).
//!
//! Verify a live service, audit saved artifacts offline, or run one
//! verified chat completion end to end.

mod args;
mod audit;
mod capture;
mod checks;
mod client;
mod send;
mod serve;
mod sessions;
#[cfg(test)]
mod spec_fixtures;
mod transcript;
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
    match args::Cli::parse().command {
        args::Command::Verify(a) => verify::run(a).await,
        args::Command::Audit(a) => audit::run(a).await,
        args::Command::Sessions(a) => sessions::run(a).await,
        args::Command::Send(a) => send::run(a).await,
        args::Command::Serve(a) => serve::run(a).await,
    }
}
