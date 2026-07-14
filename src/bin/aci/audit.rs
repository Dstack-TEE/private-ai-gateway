//! `aci audit`: the same check engine over artifacts saved to files. Offline,
//! quote collateral and the live TLS channel are honestly unavailable — those
//! checks skip (never pass) rather than assume.

use std::fs;

use private_ai_gateway::aci::types::AttestationReport;

use crate::args::AuditArgs;
use crate::checks::{
    established_identity, now_secs, parse_receipt_envelope, run_receipt_checks, run_report_checks,
    run_upstream_checks, ChannelObservation, QuoteCheckMode, ReceiptContext, ReportCheckContext,
};
use crate::transcript::Transcript;

pub async fn run(args: AuditArgs) -> Result<i32, String> {
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
            quote: QuoteCheckMode::Offline {
                reason: "quote collateral offline",
            },
            channel: ChannelObservation::NotObserved {
                reason: "offline audit: no live TLS channel observed",
            },
            explain: false,
        },
    )
    .await?;

    if let Some(receipt_path) = &args.receipt {
        let raw =
            fs::read(receipt_path).map_err(|e| format!("failed to read {receipt_path}: {e}"))?;
        let envelope = serde_json::from_slice(&raw)
            .map_err(|e| format!("failed to parse receipt JSON {receipt_path}: {e}"))?;
        let receipt = parse_receipt_envelope(envelope)?;
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
        run_receipt_checks(
            &mut transcript,
            ReceiptContext::new(
                &receipt,
                &identity,
                request_body.as_deref(),
                response_body.as_deref(),
            ),
        );
        // The session file must hold the exact served bytes (§9): the id is
        // the hash of those bytes, so a pretty-printed copy will not audit.
        let session = args
            .session
            .as_deref()
            .map(|path| fs::read(path).map_err(|e| format!("failed to read {path}: {e}")))
            .transpose()?;
        run_upstream_checks(
            &mut transcript,
            &receipt.payload,
            session.as_deref(),
            "no session record supplied",
            false,
        );
    }

    transcript.print(args.json, false)
}
