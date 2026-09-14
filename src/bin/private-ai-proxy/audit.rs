//! `aci audit`: the check engine over artifacts saved to files.
//!
//! Offline, quote collateral and the live TLS channel are honestly
//! unavailable — those checks skip (never pass) rather than assume.

use std::fs;

use private_ai_gateway::aci::types::AttestationReport;

use crate::args::AuditArgs;
use crate::checks::{
    established_identity, now_secs, parse_receipt_document, run_report_checks, run_response_checks,
    BodyDigest, ChannelEvidence, QuoteSource, ReportCheckContext, UpstreamContext,
};
use crate::transcript::Transcript;

pub async fn run(args: AuditArgs, require_production_os: bool) -> Result<i32, String> {
    let report_bytes = fs::read(&args.report)
        .map_err(|e| format!("failed to read report {}: {e}", args.report))?;
    let report: AttestationReport = serde_json::from_slice(&report_bytes)
        .map_err(|e| format!("failed to parse report JSON: {e}"))?;

    let mut transcript = Transcript::default();
    run_report_checks(
        &mut transcript,
        &report,
        ReportCheckContext {
            nonce: args.nonce.as_deref(),
            now_secs: now_secs(),
            expiry_skipped: args.skip_expiry,
            quote: QuoteSource::Offline {
                reason: "quote collateral offline",
            },
            channel: ChannelEvidence::Unobservable {
                reason: "offline audit: no live TLS channel observed",
            },
            accepted_composes: &args.accepted_composes,
            require_production_os,
            explain: false,
        },
    )
    .await?;

    if let Some(receipt_path) = &args.receipt {
        let raw =
            fs::read(receipt_path).map_err(|e| format!("failed to read {receipt_path}: {e}"))?;
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
            .map(|path| fs::read(path).map_err(|e| format!("failed to read {path}: {e}")))
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

    transcript.print(args.json, false)
}
