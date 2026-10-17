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

use std::io::{self, Write};

/// Keep a human-readable field on one line and neutralize terminal controls.
/// JSON and captured wire bytes do not use this presentation helper.
pub fn safe(value: &str) -> String {
    sanitize(value, false)
}

/// Preserve line breaks and tabs only in echoed streamed model reply text.
pub(crate) fn safe_multiline(value: &str) -> String {
    sanitize(value, true)
}

fn sanitize(value: &str, multiline: bool) -> String {
    value
        .chars()
        .map(|c| {
            if multiline && matches!(c, '\n' | '\t') {
                c
            } else if c.is_control()
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Write command output without panicking when a downstream consumer closes
/// stdout (for example, `pap verify ... | head`).
pub fn write_stdout(bytes: &[u8]) -> Result<bool, String> {
    let mut stdout = io::stdout().lock();
    match stdout.write_all(bytes).and_then(|()| stdout.flush()) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(_) => Err("Cannot write output".into()),
    }
}

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

/// Pretty JSON on stdout, the form the ACI commands print.
pub fn print_json(value: &serde_json::Value) -> Result<(), String> {
    let text =
        serde_json::to_string_pretty(value).map_err(|e| format!("failed to serialize: {e}"))?;
    write_stdout(format!("{text}\n").as_bytes())?;
    Ok(())
}

/// Select the provider used by the CLI and its verifier dependencies.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

#[cfg(test)]
mod tests {
    use super::safe;

    #[test]
    fn human_fields_neutralize_terminal_controls_and_stay_on_one_line() {
        let rendered = safe("ok\u{1b}[31m\r\u{202e}bad\n\tend");
        assert_eq!(rendered, "ok [31m  bad  end");
        assert!(!rendered.contains('\u{1b}'));
        assert!(!rendered.contains('\u{202e}'));
    }
}
