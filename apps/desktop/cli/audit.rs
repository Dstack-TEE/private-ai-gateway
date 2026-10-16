//! `private-ai-proxy audit`: the check engine over artifacts saved to files.
//!
//! Offline, quote collateral and the live TLS channel are honestly
//! unavailable — those checks skip (never pass) rather than assume.

use std::fs::{self, File};
use std::io::{self, Read};

use crate::aci::types::AttestationReport;

use crate::args::AuditArgs;
use crate::checks::{
    established_identity, parse_receipt_document, run_report_checks, run_response_checks,
    BodyDigest, ChannelEvidence, QuoteSource, ReportCheckContext, UpstreamContext,
};
use crate::client::{MAX_ATTESTATION_RESPONSE_BYTES, MAX_RECEIPT_BYTES};
use crate::transcript::Transcript;

fn read_artifact(path: &str, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("artifact exceeds maximum byte size ({limit})"),
        ));
    }
    Ok(bytes)
}

pub async fn run(args: AuditArgs) -> Result<i32, String> {
    let report_bytes = read_artifact(&args.report, MAX_ATTESTATION_RESPONSE_BYTES)
        .map_err(|e| format!("failed to read report {}: {e}", args.report))?;
    let report: AttestationReport = serde_json::from_slice(&report_bytes)
        .map_err(|e| format!("failed to parse report JSON: {e}"))?;

    let mut transcript = Transcript::default();
    run_report_checks(
        &mut transcript,
        &report,
        ReportCheckContext {
            nonce: args.nonce.as_deref(),
            now_secs: desktop_core::now_secs(),
            expiry_skipped: args.skip_expiry,
            quote: QuoteSource::Offline {
                reason: "quote collateral offline",
            },
            channel: ChannelEvidence::Unobservable {
                reason: "offline audit: no live TLS channel observed",
            },
            policy: &args.policy.verifier_policy()?,
            explain: false,
        },
    )
    .await?;

    if let Some(receipt_path) = &args.receipt {
        let raw = read_artifact(receipt_path, MAX_RECEIPT_BYTES)
            .map_err(|e| format!("failed to read {receipt_path}: {e}"))?;
        let envelope = serde_json::from_slice(&raw)
            .map_err(|e| format!("failed to parse receipt JSON {receipt_path}: {e}"))?;
        let receipt = parse_receipt_document(envelope)?;
        let identity = established_identity(&report)?;
        let request_body = args
            .request_body
            .as_deref()
            .map(|path| fs::read(path).map_err(|e| format!("failed to read {path}: {e}")))
            .transpose()?;
        let response_body = args
            .response_body
            .as_deref()
            .map(|path| fs::read(path).map_err(|e| format!("failed to read {path}: {e}")))
            .transpose()?;
        // The session file holds the session document as served (§8). Any
        // encoding audits: the id is the hash of its JCS form (Appendix A).
        let session = args
            .session
            .as_deref()
            .map(|path| {
                read_artifact(path, MAX_ATTESTATION_RESPONSE_BYTES)
                    .map_err(|e| format!("failed to read {path}: {e}"))
            })
            .transpose()?;
        let request_digest = request_body.as_deref().map(BodyDigest::of);
        let response_digest = response_body.as_deref().map(BodyDigest::of);
        run_response_checks(
            &mut transcript,
            &receipt,
            &identity,
            request_digest.as_ref(),
            response_digest.as_ref(),
            UpstreamContext {
                session_bytes: session.as_deref(),
                no_session_reason: "no session record supplied",
                pinned: (!args.pins.is_empty()).then_some(args.pins.as_slice()),
                requires_verified: args.require_verified || !args.pins.is_empty(),
                serving: &report.service_capabilities.serving,
                required_claims: &args.require_claims,
            },
        );
    }

    transcript.print(args.json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_files_are_bounded_before_parsing() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        fs::write(path, b"{}").unwrap();
        assert_eq!(read_artifact(path, MAX_RECEIPT_BYTES).unwrap(), b"{}");
        for limit in [MAX_RECEIPT_BYTES, MAX_ATTESTATION_RESPONSE_BYTES] {
            file.as_file().set_len(limit as u64 + 1).unwrap();
            assert!(read_artifact(path, limit)
                .unwrap_err()
                .to_string()
                .contains("exceeds maximum byte size"));
        }
    }
}
