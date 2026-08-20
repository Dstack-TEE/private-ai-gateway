//! `aci curl`: verify an ACI origin, then delegate the request to curl.
//!
//! The ACI client performs the attestation checks and records the TLS leaf's
//! SPKI. The system curl handles the actual request with that SPKI pinned, so
//! uploads, downloads, streaming, authentication, and output behave like curl.

use std::ffi::{OsStr, OsString};
use std::process::{Command, ExitStatus};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use crate::args::CurlArgs;
use crate::verify::verify_service;

const CURL_SECURITY_ARGS: [&str; 5] = [
    "--proto",
    "=https",
    "--proto-redir",
    "=https",
    "--pinnedpubkey",
];

const RESERVED_LONG_OPTIONS: [&str; 8] = [
    "--pinnedpubkey",
    "--proto",
    "--proto-redir",
    "--url",
    "--config",
    "--next",
    "--location",
    "--location-trusted",
];

pub async fn run(args: CurlArgs, require_production_os: bool) -> Result<i32, String> {
    let (url, origin) = request_url_and_origin(&args.url)?;
    validate_curl_args(&args.curl_args)?;

    let verification = verify_service(
        &origin,
        None,
        &args.accepted_composes,
        require_production_os,
        false,
    )
    .await?;

    eprintln!("== ACI verification: {origin} ==");
    eprint!("{}", verification.transcript.render_human(false));
    if !verification.transcript.verified() {
        return Err("service verification failed; curl was not started (fail closed)".into());
    }

    let observed_spki = verification
        .observed_spki
        .as_deref()
        .ok_or("verified service has no observed TLS SPKI; curl was not started")?;
    let pin = curl_spki_pin(observed_spki)?;
    eprintln!("PINNED      curl -> attested TLS key");

    let command_args = curl_command_args(&url, &pin, &args.curl_args);
    run_curl(OsStr::new("curl"), &command_args)
}

fn request_url_and_origin(value: &str) -> Result<(String, String), String> {
    let url =
        reqwest::Url::parse(value).map_err(|e| format!("invalid request URL {value:?}: {e}"))?;
    if url.scheme() != "https" {
        return Err(format!(
            "request URL must use https so the attested TLS key can be pinned: {value:?}"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("request URL must not contain credentials; pass authentication to curl".into());
    }
    if url.fragment().is_some() {
        return Err("request URL must not contain a fragment".into());
    }
    if url.host_str().is_none() {
        return Err(format!("request URL {value:?} has no host"));
    }

    Ok((url.to_string(), url.origin().ascii_serialization()))
}

fn curl_spki_pin(spki_sha256_hex: &str) -> Result<String, String> {
    let digest = hex::decode(spki_sha256_hex)
        .map_err(|e| format!("attested TLS SPKI digest is not valid hex: {e}"))?;
    if digest.len() != 32 {
        return Err(format!(
            "attested TLS SPKI digest has {} bytes, expected 32",
            digest.len()
        ));
    }
    Ok(format!("sha256//{}", BASE64.encode(digest)))
}

/// Reject options that could replace the verified URL, pin, protocol policy,
/// or curl option scope. Everything else remains native curl behavior.
fn validate_curl_args(args: &[OsString]) -> Result<(), String> {
    for arg in args {
        let value = arg
            .to_str()
            .ok_or("curl arguments must be valid UTF-8 so security options can be validated")?;
        if value.starts_with('-') && !value.starts_with("--") && value != "-" && value.len() != 2 {
            return Err(format!(
                "curl argument {value:?} groups a short option or attaches its value; \
                 pass each short option and value as separate arguments so aci can validate them"
            ));
        }
        let long_name = value.split_once('=').map_or(value, |(name, _)| name);
        // curl accepts unambiguous long-option abbreviations. Reject every
        // prefix of a reserved option so `--pinnedp`, for example, cannot
        // replace the attested pin on curl versions where it is unambiguous.
        let reserved_long = long_name.starts_with("--")
            && RESERVED_LONG_OPTIONS
                .iter()
                .any(|reserved| reserved.starts_with(long_name));
        let reserved = reserved_long || matches!(value, "-K" | "-:" | "-L");

        if reserved {
            return Err(format!(
                "curl argument {value:?} is managed by aci and cannot be overridden"
            ));
        }
    }
    Ok(())
}

fn curl_command_args(url: &str, pin: &str, user_args: &[OsString]) -> Vec<OsString> {
    let mut args = Vec::with_capacity(7 + user_args.len());
    // `-q` must be curl's first argument to disable the implicit curlrc, which
    // could otherwise introduce another URL or transfer scope before our pin.
    args.push(OsString::from("-q"));
    args.extend(CURL_SECURITY_ARGS.map(OsString::from));
    args.push(OsString::from(pin));
    args.extend(user_args.iter().cloned());
    args.push(OsString::from(url));
    args
}

fn run_curl(curl_binary: &OsStr, args: &[OsString]) -> Result<i32, String> {
    let status = Command::new(curl_binary)
        .args(args)
        .status()
        .map_err(|e| format!("failed to start system curl: {e}"))?;
    Ok(exit_code(status))
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn derives_origin_from_https_request_url() {
        let (url, origin) =
            request_url_and_origin("https://Example.COM:8443/v1/chat/completions?q=1").unwrap();
        assert_eq!(url, "https://example.com:8443/v1/chat/completions?q=1");
        assert_eq!(origin, "https://example.com:8443");
    }

    #[test]
    fn rejects_urls_that_cannot_be_safely_pinned() {
        assert!(request_url_and_origin("http://example.com/v1/models").is_err());
        assert!(request_url_and_origin("https://user@example.com/v1/models").is_err());
        assert!(request_url_and_origin("https://example.com/v1/models#response").is_err());
    }

    #[test]
    fn converts_hex_digest_to_curl_pin() {
        assert_eq!(
            curl_spki_pin(&"00".repeat(32)).unwrap(),
            "sha256//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
        );
        assert!(curl_spki_pin("00").is_err());
        assert!(curl_spki_pin("not-hex").is_err());
    }

    #[test]
    fn rejects_curl_options_that_can_escape_the_verified_transfer() {
        for arg in [
            "--pinnedpubkey",
            "--pinnedpubkey=sha256//other",
            "--pinnedp",
            "--proto",
            "--proto-r=all",
            "--proto-redir=all",
            "--url=https://other.example",
            "--confi",
            "--config",
            "--next",
            "-:",
            "--location",
            "--locatio",
            "--location-trusted",
            "--location-t",
            "-L",
        ] {
            let err = validate_curl_args(&[OsString::from(arg)]).unwrap_err();
            assert!(err.contains("managed by aci"), "{arg}: {err}");
        }
        validate_curl_args(&[
            OsString::from("--header"),
            OsString::from("Authorization: Bearer token"),
            OsString::from("--data-binary"),
            OsString::from("@request.json"),
            OsString::from("--upload-file"),
            OsString::from("payload.bin"),
            OsString::from("--proto-default"),
            OsString::from("https"),
        ])
        .unwrap();
    }

    #[test]
    fn rejects_grouped_and_attached_short_options() {
        for arg in [
            "-sS",
            "-Kfile",
            "-sKconfig",
            "-sL",
            "-XPOST",
            "-Haccept:json",
        ] {
            let err = validate_curl_args(&[OsString::from(arg)]).unwrap_err();
            assert!(err.contains("separate arguments"), "{arg}: {err}");
        }
        validate_curl_args(&[
            OsString::from("-s"),
            OsString::from("-S"),
            OsString::from("-X"),
            OsString::from("POST"),
            OsString::from("-H"),
            OsString::from("accept: application/json"),
        ])
        .unwrap();
    }

    #[test]
    fn builds_a_curl_command_with_config_disabled_and_pin_applied() {
        let args = curl_command_args(
            "https://example.com/v1/models",
            "sha256//pin",
            &[OsString::from("--silent")],
        );
        let expected = [
            "-q",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--pinnedpubkey",
            "sha256//pin",
            "--silent",
            "https://example.com/v1/models",
        ];
        assert_eq!(
            args.iter().map(OsString::as_os_str).collect::<Vec<_>>(),
            expected.map(OsStr::new)
        );
    }

    #[cfg(unix)]
    #[test]
    fn runs_external_curl_and_preserves_its_exit_code() {
        let directory = tempfile::tempdir().unwrap();
        let fake_curl = directory.path().join("curl");
        fs::write(
            &fake_curl,
            "#!/bin/sh\n[ \"$1\" = \"-q\" ] || exit 91\n[ \"$2\" = \"--silent\" ] || exit 92\nexit 23\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&fake_curl).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&fake_curl, permissions).unwrap();

        let code = run_curl(
            fake_curl.as_os_str(),
            &[OsString::from("-q"), OsString::from("--silent")],
        )
        .unwrap();
        assert_eq!(code, 23);
    }

    #[test]
    fn reports_when_curl_cannot_be_started() {
        let err = run_curl(
            OsStr::new("/path/that/does/not/contain/curl"),
            &[OsString::from("--version")],
        )
        .unwrap_err();
        assert!(err.contains("failed to start system curl"), "{err}");
    }
}
