//! Private AI Proxy's ACI relying-party side: verification and audit
//! primitives, the ACI commands, and the verifying proxy that both the
//! standalone `serve` command and the managed backend run.

pub mod aci;
pub mod args;
pub mod audit;
mod capture;
mod checks;
mod client;
pub mod send;
pub mod serve;
pub mod sessions;
#[cfg(test)]
mod spec_fixtures;
mod transcript;
pub mod verify;

/// Select the provider used by the CLI and its verifier dependencies.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
