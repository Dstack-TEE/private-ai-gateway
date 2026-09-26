//! ACI command arguments for Private AI Proxy, built on clap's derive API.

use std::ffi::OsString;

use clap::{Args, Subcommand};

use crate::aci::verifier::{CustodyPolicy, CustodyPolicyError};
use crate::checks::{RequiredClaim, VerifierPolicy};

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(
        about = "Fetch /v1/aci/attestation with a fresh nonce, run the spec 9.1 checks \
                 under the dstack tdx verifier policy (spec 1.3), and print a \
                 verification transcript. Exits 0 only if the verdict is VERIFIED."
    )]
    Verify(VerifyArgs),
    #[command(
        about = "Offline verification of saved artifacts using the same transcript engine as verify."
    )]
    Audit(AuditArgs),
    #[command(
        about = "Verify the service (fail closed), list its current attested sessions, \
                 and run the spec 9.2 audit on each — the ids that pass are what a \
                 client pins (spec 5.3)."
    )]
    Sessions(SessionsArgs),
    #[command(
        about = "Verify the service (fail closed), send one chat completion over an \
                 SPKI-pinned connection, then fetch and verify its receipt. The API key \
                 is read from stdin with --api-key-stdin or from the ACI_API_KEY \
                 environment variable."
    )]
    Send(SendArgs),
    #[command(
        about = "Verify the target's ACI service (fail closed), then run the system curl \
                 with the attested TLS key pinned. Exits 125 when pap refuses the request, \
                 otherwise with curl's exit code."
    )]
    Curl(CurlArgs),
    #[command(
        about = "Local verifying proxy (default 127.0.0.1:4180, plain HTTP on localhost). \
                 Verifies the service on startup and refuses to start unless VERIFIED, \
                 accepts plaintext API requests only, forwards them over the pinned attested \
                 TLS channel, and verifies each POST response's receipt after the fact."
    )]
    Serve(ServeArgs),
}

/// The verifier policy (spec 1.3) every command that appraises a report runs
/// under. Names follow the gateway's upstream policy configuration.
#[derive(Debug, Default, Args)]
pub struct PolicyArgs {
    #[arg(
        long = "accept-compose",
        value_name = "HEX",
        help = "Compose hash to accept (spec 1.3 verifier policy); repeatable. Without \
                it the compose measurement is verified and reported, and you appraise \
                the provenance yourself."
    )]
    pub accepted_composes: Vec<String>,
    #[arg(
        long = "accept-subject",
        value_name = "app-id:0xHEX",
        help = "Measured dstack app-id to accept for key custody (spec 9.1(5)); \
                repeatable. Custody is checked when this and \
                --accept-dstack-kms-root-public-key are given; with neither, id-5 is \
                skipped."
    )]
    pub accepted_subjects: Vec<String>,
    #[arg(
        long = "accept-dstack-kms-root-public-key",
        value_name = "HEX",
        help = "dstack KMS root public key the receipt-key custody chain must end at \
                (spec 3.3, 9.1(5)); repeatable."
    )]
    pub accepted_dstack_kms_root_public_keys: Vec<String>,
}

impl PolicyArgs {
    /// Validate the flags once, before any report is fetched.
    pub fn verifier_policy(&self) -> Result<VerifierPolicy, String> {
        let custody_configured = !(self.accepted_subjects.is_empty()
            && self.accepted_dstack_kms_root_public_keys.is_empty());
        let custody = custody_configured
            .then(|| {
                CustodyPolicy::new(
                    self.accepted_subjects.clone(),
                    self.accepted_dstack_kms_root_public_keys.clone(),
                )
            })
            .transpose()
            .map_err(|e| match e {
                CustodyPolicyError::EmptySubjects => {
                    "--accept-dstack-kms-root-public-key needs --accept-subject".to_string()
                }
                CustodyPolicyError::EmptyKmsRoots => {
                    "--accept-subject needs --accept-dstack-kms-root-public-key".to_string()
                }
                CustodyPolicyError::InvalidSubject(subject) => {
                    format!("invalid --accept-subject {subject:?}: expected app-id:0x<hex>")
                }
                CustodyPolicyError::InvalidKmsRootPublicKey(e) => {
                    format!("invalid --accept-dstack-kms-root-public-key: {e}")
                }
            })?;
        Ok(VerifierPolicy {
            accepted_composes: self.accepted_composes.clone(),
            custody,
        })
    }
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    #[arg(help = "Base URL of the ACI service to verify.")]
    pub base_url: String,
    #[command(flatten)]
    pub policy: PolicyArgs,
    #[arg(
        long,
        help = "Nonce to send with the attestation request; a fresh random one is \
                generated when omitted."
    )]
    pub nonce: Option<String>,
    #[arg(
        long,
        help = "Print the verification transcript as JSON instead of the human-readable form."
    )]
    pub json: bool,
    #[arg(
        long,
        help = "Print a one-line explanation for every check, not just failures."
    )]
    pub explain: bool,
}

#[derive(Debug, Default, Args)]
pub struct AuditArgs {
    #[arg(
        long,
        value_name = "FILE",
        help = "Path to the saved spec 9.1 attestation report JSON."
    )]
    pub report: String,
    #[command(flatten)]
    pub policy: PolicyArgs,
    #[arg(
        long,
        value_name = "FILE",
        help = "Path to the saved receipt document (spec 7.2) to verify alongside the report."
    )]
    pub receipt: Option<String>,
    #[arg(
        long,
        help = "Nonce the report was originally fetched with, to check the id-3 freshness check (spec 9.1)."
    )]
    pub nonce: Option<String>,
    #[arg(
        long = "request-body",
        value_name = "FILE",
        help = "Path to the saved raw request bytes, checked against the receipt (spec 9.3)."
    )]
    pub request_body: Option<String>,
    #[arg(
        long = "response-body",
        value_name = "FILE",
        help = "Path to the saved raw response bytes, checked against the receipt (spec 9.3)."
    )]
    pub response_body: Option<String>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Path to the saved session document (spec 8) the receipt cites."
    )]
    pub session: Option<String>,
    #[arg(
        long = "require-claim",
        value_name = "NAME[=SOURCE]",
        value_parser = RequiredClaim::parse,
        help = "Claim the audited session must assert (spec 9.2(3)), as NAME or \
                NAME=SOURCE, e.g. tee_attested=hardware_proven; repeatable."
    )]
    pub require_claims: Vec<RequiredClaim>,
    #[arg(
        long = "pin",
        value_name = "SESSION_ID",
        value_parser = session_id,
        help = "Session id (spec 5.3) the cited session must match (spec 9.3(6)); repeatable."
    )]
    pub pins: Vec<String>,
    #[arg(
        long = "require-verified",
        help = "Apply the spec 9.3(5)-(6) strict failures offline."
    )]
    pub require_verified: bool,
    #[arg(long = "skip-expiry", help = "Skip the report expiry check (id-3).")]
    pub skip_expiry: bool,
    #[arg(
        long,
        help = "Print the verification transcript as JSON instead of the human-readable form."
    )]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct SendArgs {
    #[arg(help = "Base URL of the ACI service to send the request to.")]
    pub base_url: String,
    #[command(flatten)]
    pub policy: PolicyArgs,
    #[arg(
        long,
        value_name = "MODEL",
        help = "Model id to request; defaults to the first model the service advertises."
    )]
    pub model: Option<String>,
    #[arg(
        long,
        value_name = "TEXT",
        help = "Prompt text to send; defaults to a canned greeting."
    )]
    pub prompt: Option<String>,
    #[arg(
        long = "api-key-stdin",
        help = "Read the bearer API key from stdin; otherwise it is read from the \
                ACI_API_KEY environment variable."
    )]
    pub api_key_stdin: bool,
    /// Deprecated: a key in argv is visible to other local processes. Removed in 0.3.
    #[arg(
        long = "api-key",
        value_name = "KEY",
        hide = true,
        conflicts_with = "api_key_stdin"
    )]
    pub api_key: Option<String>,
    #[arg(long = "no-stream", help = "Request a non-streaming chat completion.")]
    pub no_stream: bool,
    #[arg(long, help = "Print the verification transcript and result as JSON.")]
    pub json: bool,
    #[arg(
        long = "allow-unverified",
        conflicts_with = "sessions",
        help = "Drop the default provider.aci_verified demand (spec 5.3). Cannot be \
                combined with --session, which implies verified serving."
    )]
    pub allow_unverified: bool,
    #[arg(
        long = "require-claim",
        value_name = "NAME[=SOURCE]",
        value_parser = RequiredClaim::parse,
        help = "Claim the audited session must assert (spec 9.2(3)), as NAME or \
                NAME=SOURCE, e.g. tee_attested=hardware_proven; repeatable."
    )]
    pub require_claims: Vec<RequiredClaim>,
    #[arg(
        long = "session",
        value_name = "SESSION_ID",
        value_parser = session_id,
        help = "Session id (spec 5.3) to pin via provider.aci_session_ids; the receipt's \
                cited session is checked against this list (spec 9.3(6)); repeatable. \
                Implies verified serving."
    )]
    pub sessions: Vec<String>,
}

#[derive(Debug, Args)]
pub struct CurlArgs {
    #[arg(help = "HTTPS URL to request after its ACI service has been verified.")]
    pub url: String,
    #[command(flatten)]
    pub policy: PolicyArgs,
    #[arg(
        long,
        help = "Print the verification transcript as JSON on stderr; stdout stays curl's."
    )]
    pub json: bool,
    #[arg(
        last = true,
        allow_hyphen_values = true,
        value_name = "CURL_ARG",
        help = "Supported request options passed to system curl. Put them after `--`; \
                pap owns the URL and transport-security options."
    )]
    pub curl_args: Vec<OsString>,
}

#[derive(Debug, Args)]
pub struct SessionsArgs {
    #[arg(help = "Base URL of the ACI service whose attested sessions to audit.")]
    pub base_url: String,
    #[command(flatten)]
    pub policy: PolicyArgs,
    #[arg(
        long,
        value_name = "MODEL",
        help = "Only list sessions for upstreams serving this model."
    )]
    pub model: Option<String>,
    #[arg(
        long = "require-claim",
        value_name = "NAME[=SOURCE]",
        value_parser = RequiredClaim::parse,
        help = "Claim an accepted session must assert (spec 9.2(3)), as NAME or \
                NAME=SOURCE, e.g. tee_attested=hardware_proven; repeatable."
    )]
    pub require_claims: Vec<RequiredClaim>,
    #[arg(long, help = "Print the audited sessions as JSON.")]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ServeArgs {
    #[arg(help = "Base URL of the ACI service to proxy to.")]
    pub base_url: String,
    #[command(flatten)]
    pub policy: PolicyArgs,
    #[arg(
        long,
        value_name = "ADDR:PORT",
        help = "Local address to listen on (default 127.0.0.1:4180)."
    )]
    pub listen: Option<String>,
    #[arg(
        long,
        value_name = "ADDR:PORT",
        help = "Local control endpoint for on-demand receipt verification \
                (default 127.0.0.1:4183)."
    )]
    pub control: Option<String>,
    #[arg(
        long = "json-events",
        help = "Emit the serve lifecycle and request activity as JSON Lines on stdout. \
                Human-readable diagnostics remain on stderr."
    )]
    pub json_events: bool,
    #[arg(
        long = "allow-unverified",
        help = "Drop the default provider.aci_verified demand (spec 5.3)."
    )]
    pub allow_unverified: bool,
    #[arg(
        long = "session",
        value_name = "SESSION_ID",
        value_parser = session_id,
        conflicts_with_all = ["require_claims", "allow_unverified"],
        help = "Session id (spec 5.3) accepted by the local policy; repeatable. \
                Each POST uses the intersection with its own pins, or this set \
                when it supplies none. A disjoint request is rejected locally. \
                Implies verified serving."
    )]
    pub sessions: Vec<String>,
    #[arg(
        long = "require-claim",
        value_name = "NAME[=SOURCE]",
        value_parser = RequiredClaim::parse,
        conflicts_with = "allow_unverified",
        help = "Derive the pinned session set from the service's current attested \
                sessions: only sessions asserting this claim are pinned (spec 9.2(3)); \
                repeatable. The set refreshes when the service refuses a pin (HTTP 412)."
    )]
    pub require_claims: Vec<RequiredClaim>,
}

/// A bare 64-character lowercase-hex session id (spec 5.3, spec 8).
fn session_id(value: &str) -> Result<String, String> {
    let is_valid = value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if is_valid {
        Ok(value.to_string())
    } else {
        Err(format!("{value:?} is not a 64-hex session id (spec 5.3)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::FromArgMatches;

    // Only `session_id` is ours; clap's own parsing needs no test, and
    // `RequiredClaim::parse` is covered in checks.rs.
    #[test]
    fn session_id_accepts_only_bare_64_hex() {
        assert!(session_id(&"a".repeat(64)).is_ok());
        assert!(session_id(&"A".repeat(64)).is_err(), "uppercase");
        assert!(session_id(&"a".repeat(63)).is_err(), "too short");
        assert!(
            session_id(&format!("sha256:{}", "a".repeat(64))).is_err(),
            "prefixed"
        );
        let err = session_id("not-hex").unwrap_err();
        assert!(err.contains("spec 5.3"), "{err}");
    }

    // The generator point, a valid compressed secp256k1 key.
    const ROOT: &str = "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";

    fn policy(subjects: &[&str], roots: &[&str]) -> Result<VerifierPolicy, String> {
        PolicyArgs {
            accepted_composes: Vec::new(),
            accepted_subjects: subjects.iter().map(|s| s.to_string()).collect(),
            accepted_dstack_kms_root_public_keys: roots.iter().map(|s| s.to_string()).collect(),
        }
        .verifier_policy()
    }

    #[test]
    fn custody_flags_build_a_policy_or_name_the_missing_flag() {
        assert!(policy(&[], &[]).unwrap().custody.is_none());
        assert!(policy(&["app-id:0xAB"], &[ROOT]).unwrap().custody.is_some());

        let err = policy(&[], &[ROOT]).unwrap_err();
        assert_eq!(
            err,
            "--accept-dstack-kms-root-public-key needs --accept-subject"
        );
        let err = policy(&["app-id:0xab"], &[]).unwrap_err();
        assert_eq!(
            err,
            "--accept-subject needs --accept-dstack-kms-root-public-key"
        );
        let err = policy(&["0xab"], &[ROOT]).unwrap_err();
        assert_eq!(
            err,
            r#"invalid --accept-subject "0xab": expected app-id:0x<hex>"#
        );
        let err = policy(&["app-id:0xab"], &["02zz"]).unwrap_err();
        assert!(
            err.starts_with("invalid --accept-dstack-kms-root-public-key: "),
            "{err}"
        );
    }

    #[test]
    fn curl_args_after_separator_are_passed_through() {
        let matches = Command::augment_subcommands(clap::Command::new("pap"))
            .try_get_matches_from([
                "pap",
                "curl",
                "https://example.com/v1/chat/completions",
                "--accept-compose",
                "abcd",
                "--json",
                "--",
                "--json",
                "{}",
                "--data-binary",
                "@request.json",
            ])
            .unwrap();
        let Command::Curl(args) = Command::from_arg_matches(&matches).unwrap() else {
            panic!("expected curl command");
        };
        assert_eq!(args.policy.accepted_composes, ["abcd"]);
        assert!(args.json);
        assert_eq!(
            args.curl_args,
            ["--json", "{}", "--data-binary", "@request.json"].map(OsString::from)
        );
    }
}
