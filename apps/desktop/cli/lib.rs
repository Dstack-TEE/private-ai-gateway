//! Private AI Proxy's ACI relying-party side: verification and audit
//! primitives, the ACI commands, and the verifying proxy that both the
//! standalone `serve` command and the managed backend run.

pub mod aci;
pub mod args;
pub mod audit;
mod capture;
mod checks;
mod client;
pub mod curl;
pub mod send;
pub mod serve;
pub mod sessions;
#[cfg(test)]
mod spec_fixtures;
mod transcript;
pub mod verify;

/// Read an API key piped to stdin, like `docker login --password-stdin`:
/// bounded, UTF-8, surrounding whitespace such as `echo`'s newline ignored.
pub fn read_api_key(input: impl std::io::Read) -> Result<String, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    input
        .take(514)
        .read_to_end(&mut bytes)
        .map_err(|_| "Cannot read credential from stdin")?;
    if bytes.len() > 513 {
        return Err("Credential input exceeds limit".into());
    }
    let key = String::from_utf8(bytes).map_err(|_| "Credential must be UTF-8")?;
    desktop_core::config::validate_api_key(&key)
}

/// Select the provider used by the CLI and its verifier dependencies.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}
