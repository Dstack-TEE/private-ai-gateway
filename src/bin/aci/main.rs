//! `aci` — reference client for the Attested Confidential Inference protocol
//! (`spec/aci.md`). Verify a live service, audit saved artifacts offline, or
//! run one verified chat completion end to end.

mod args;
mod audit;
mod chat;
mod checks;
mod client;
mod serve;
#[cfg(test)]
mod spec_fixtures;
mod transcript;
mod verify;

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
    match args::parse(std::env::args().skip(1))? {
        args::Command::Verify(a) => verify::run(a).await,
        args::Command::Audit(a) => audit::run(a).await,
        args::Command::Chat(a) => chat::run(a).await,
        args::Command::Serve(a) => serve::run(a).await,
    }
}
