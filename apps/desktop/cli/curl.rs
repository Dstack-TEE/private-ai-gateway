//! `pap curl`: verify an ACI origin, then delegate one request to system curl.
//!
//! The ACI client runs the attestation checks. System curl then makes the
//! request with the TLS keys the verifier established pinned, so supported
//! single-request options, streaming, authentication, and output behave like
//! curl.

use std::ffi::{OsStr, OsString};
use std::process::{Command, ExitStatus};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

use crate::args::CurlArgs;
use crate::transcript::Transcript;
use crate::verify::verify_service;

/// Exit code when pap refuses the request or cannot run curl. curl's own
/// codes stay below it and signals map above it.
pub const PAP_FAILURE_EXIT_CODE: i32 = 125;

pub async fn run(args: CurlArgs, require_production_os: bool) -> Result<i32, String> {
    let (url, origin) = request_url_and_origin(&args.url)?;
    validate_curl_args(&args.curl_args)?;
    let policy = args.policy.verifier_policy()?;

    let verification = verify_service(&origin, None, &policy, require_production_os, false).await?;
    // stdout belongs to curl's response, so the transcript goes to stderr.
    if args.json {
        let transcript = serde_json::to_string(&verification.transcript.to_json(false))
            .map_err(|e| format!("failed to serialize transcript: {e}"))?;
        eprintln!("{transcript}");
    } else {
        eprintln!("== ACI verification: {origin} ==");
        eprint!("{}", verification.transcript.render_human(false));
    }

    let command_args = pinned_curl_args(
        &verification.transcript,
        &verification.attested_spkis(),
        &url,
        &args.curl_args,
    )?;
    if !args.json {
        eprintln!("PINNED      curl -> attested TLS key");
    }
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

/// The curl arguments for a VERIFIED transcript, pinned to the TLS keys its
/// channel check established; any other outcome means curl is not started.
fn pinned_curl_args(
    transcript: &Transcript,
    tls_pins: &[String],
    url: &str,
    user_args: &[OsString],
) -> Result<Vec<OsString>, String> {
    if !transcript.verified() {
        return Err("service verification failed; curl was not started (fail closed)".into());
    }
    if tls_pins.is_empty() {
        return Err("verification established no TLS key to pin; curl was not started".into());
    }
    let pin = tls_pins
        .iter()
        .map(|spki| curl_spki_pin(spki))
        .collect::<Result<Vec<_>, _>>()?
        .join(";");

    let fixed = [
        // `-q` must come first: it disables the implicit curlrc, which could
        // otherwise add another URL or transfer scope around the pin.
        "-q",
        // A URL containing `[1-3]` or `{a,b}` stays one request.
        "--globoff",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--pinnedpubkey",
        &pin,
    ];
    let mut args: Vec<OsString> = fixed.iter().map(OsString::from).collect();
    args.extend(user_args.iter().cloned());
    args.push(OsString::from(url));
    Ok(args)
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

/// Accept only one-transfer options whose meaning cannot replace the URL,
/// TLS pin, protocol policy, or transfer scope. In particular, a positional
/// argument is never forwarded as a second URL.
fn validate_curl_args(args: &[OsString]) -> Result<(), String> {
    let mut values = args.iter();
    while let Some(arg) = values.next() {
        let value = arg.to_str().ok_or("curl arguments must be valid UTF-8")?;
        if matches!(
            value,
            "--fail"
                | "--fail-with-body"
                | "--no-buffer"
                | "--silent"
                | "--show-error"
                | "--include"
                | "--verbose"
                | "--compressed"
                | "--head"
                | "-f"
                | "-s"
                | "-S"
                | "-i"
                | "-v"
                | "-I"
        ) {
            continue;
        }
        if matches!(
            value,
            "--header"
                | "--data"
                | "--data-raw"
                | "--data-binary"
                | "--json"
                | "--request"
                | "--output"
                | "--upload-file"
                | "--form"
                | "--max-time"
                | "--connect-timeout"
                | "-H"
                | "-d"
                | "-X"
                | "-o"
                | "-T"
                | "-F"
        ) {
            values
                .next()
                .ok_or_else(|| format!("curl option {value:?} needs a value"))?;
            continue;
        }
        return Err(format!(
            "curl argument {value:?} is not supported by pap curl; use a single URL and supported request options"
        ));
    }
    Ok(())
}

fn run_curl(curl_binary: &OsStr, args: &[OsString]) -> Result<i32, String> {
    let status = Command::new(curl_binary)
        .args(args)
        .status()
        .map_err(|e| format!("failed to start system curl: {e}"))?;
    Ok(exit_code(status))
}

/// curl's exit code, or 128 + the signal that killed it, as shells report it.
fn exit_code(status: ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return 128 + signal;
        }
    }
    status
        .code()
        .expect("a process not killed by a signal has an exit code")
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{fs, os::unix::fs::PermissionsExt, path::Path, path::PathBuf};

    use super::*;
    use crate::transcript::{ID_2, ID_6};

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    fn verified() -> Transcript {
        let mut transcript = Transcript::default();
        transcript.pass(ID_2, "ok");
        transcript.pass(ID_6, "ok");
        transcript
    }

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
            "https://other.example/",
            "--insecure",
            "-k",
            "--resolve",
            "--connect-to",
            "--request-target",
            "--proxy",
            "-x",
            "--no-globoff",
        ] {
            let err = validate_curl_args(&args(&[arg])).unwrap_err();
            assert!(err.contains("not supported"), "{arg}: {err}");
        }
        validate_curl_args(&args(&[
            "--header",
            "Authorization: Bearer token",
            "--data-binary",
            "@request.json",
            "--upload-file",
            "payload.bin",
        ]))
        .unwrap();
    }

    #[test]
    fn rejects_transport_overrides_mixed_with_allowed_options() {
        for (rejected, arguments) in [
            (
                "--insecure",
                &["--silent", "--header", "accept: */*", "--insecure"][..],
            ),
            (
                "-x",
                &["-H", "accept: */*", "-x", "http://proxy:8080", "-s"],
            ),
            (
                "--connect-to",
                &[
                    "--fail",
                    "--connect-to",
                    "example.com:443:other.example:443",
                    "--data",
                    "{}",
                ],
            ),
        ] {
            let err = validate_curl_args(&args(arguments)).unwrap_err();
            assert!(err.contains(&format!("{rejected:?}")), "{rejected}: {err}");
        }
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
            let err = validate_curl_args(&args(&[arg])).unwrap_err();
            assert!(err.contains("not supported"), "{arg}: {err}");
        }
        validate_curl_args(&args(&[
            "-s",
            "-S",
            "-X",
            "POST",
            "-H",
            "accept: application/json",
        ]))
        .unwrap();
    }

    #[test]
    fn builds_one_pinned_request_with_config_and_globbing_disabled() {
        let url = "https://example.com/v1/items/[1-100]/{a,b}";
        let command = pinned_curl_args(
            &verified(),
            &["00".repeat(32), "ff".repeat(32)],
            url,
            &args(&["--silent"]),
        )
        .unwrap();
        assert_eq!(
            command,
            args(&[
                "-q",
                "--globoff",
                "--proto",
                "=https",
                "--proto-redir",
                "=https",
                "--pinnedpubkey",
                "sha256//AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=;\
                 sha256////////////////////////////////////////////8=",
                "--silent",
                url,
            ])
        );
    }

    #[test]
    fn curl_is_not_started_unless_verification_passed() {
        let url = "https://example.com/v1/models";
        let mut failed = verified();
        failed.fail(ID_6, "observed SPKI is not the declared entry");
        for transcript in [Transcript::default(), failed] {
            let err = pinned_curl_args(&transcript, &["00".repeat(32)], url, &[]).unwrap_err();
            assert!(err.contains("curl was not started"), "{err}");
        }
        let err = pinned_curl_args(&verified(), &[], url, &[]).unwrap_err();
        assert!(err.contains("curl was not started"), "{err}");
    }

    #[cfg(unix)]
    fn fake_curl(directory: &Path, script: &str) -> PathBuf {
        let path = directory.join("curl");
        fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn runs_external_curl_and_preserves_its_exit_code() {
        let directory = tempfile::tempdir().unwrap();
        let curl = fake_curl(
            directory.path(),
            "[ \"$1\" = \"-q\" ] || exit 91\n[ \"$2\" = \"--silent\" ] || exit 92\nexit 23",
        );
        let code = run_curl(curl.as_os_str(), &args(&["-q", "--silent"])).unwrap();
        assert_eq!(code, 23);
    }

    #[cfg(unix)]
    #[test]
    fn reports_a_curl_killed_by_a_signal_as_128_plus_the_signal() {
        let directory = tempfile::tempdir().unwrap();
        let curl = fake_curl(directory.path(), "kill -TERM $$");
        assert_eq!(run_curl(curl.as_os_str(), &[]).unwrap(), 128 + 15);
    }

    #[test]
    fn reports_when_curl_cannot_be_started() {
        let err = run_curl(
            OsStr::new("/path/that/does/not/contain/curl"),
            &args(&["--version"]),
        )
        .unwrap_err();
        assert!(err.contains("failed to start system curl"), "{err}");
    }
}
