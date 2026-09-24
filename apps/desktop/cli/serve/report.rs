//! Rendering: the one-line receipt summary, console request lines and the
//! `--json-events` stream.

use std::io::{self, Write};

use desktop_runtime::verifier_session::{IdentityEvent, IdentitySourceProvenance, VerifierEvent};
use serde_json::{json, Value};

use super::RequestOutcome;
use crate::aci::types::AttestationReport;
use crate::checks::EstablishedIdentity;
use crate::transcript::{Status, Transcript};

/// One-line receipt summary: signature, wire hash, and the asserted upstream
/// claims (e.g. `signature ok, wire hash ok, upstream tee_attested asserted (hardware_proven)`).
pub(super) fn summarize(transcript: &Transcript, session: Option<&Value>, serving: &str) -> String {
    let mut parts = vec![
        check_clause(transcript, "receipt-1", "signature"),
        check_clause(transcript, "receipt-4", "wire hash"),
        upstream_clause(transcript, session, serving),
    ];
    parts.retain(|part| !part.is_empty());
    parts.join(", ")
}

fn check_clause(transcript: &Transcript, id: &str, label: &str) -> String {
    match status_of(transcript, id) {
        Some(Status::Pass) => format!("{label} ok"),
        Some(Status::Fail) => format!("{label} FAILED"),
        Some(Status::Skip) => format!("{label} skipped"),
        _ => String::new(),
    }
}

/// `upstream <name status (source)>...` over the asserted claims of the cited
/// session (§8.3), or a loud clause if the shallow audit (upstream-1) did not pass.
fn upstream_clause(transcript: &Transcript, session: Option<&Value>, serving: &str) -> String {
    // §4.1/§5.3: a direct service has no upstream hop — "UNVERIFIED" would
    // misread the workload the client itself verified.
    if serving == "direct" {
        return "direct service, no upstream hop".to_string();
    }
    if status_of(transcript, "upstream-1") != Some(Status::Pass) {
        return "upstream UNVERIFIED".to_string();
    }
    let claims = session
        .and_then(|record| record.get("claims"))
        .and_then(Value::as_object);
    let asserted: Vec<String> = claims
        .into_iter()
        .flatten()
        .filter(|(name, _)| name.as_str() != "extra")
        .filter_map(|(name, claim)| {
            // Appendix B: an unrecognized status or source is treated as
            // `unknown`, so it is never presented as a claim of record.
            let status = match claim.get("status").and_then(Value::as_str)? {
                status @ ("asserted" | "refuted") => status,
                _ => return None,
            };
            match claim.get("source").and_then(Value::as_str) {
                Some(
                    source @ ("hardware_proven" | "verifier_derived" | "provider_asserted"
                    | "operator_asserted"),
                ) => Some(format!("{name} {status} ({source})")),
                Some(_) => None,
                None => Some(format!("{name} {status}")),
            }
        })
        .collect();
    if asserted.is_empty() {
        "upstream verified".to_string()
    } else {
        format!("upstream {}", asserted.join(", "))
    }
}

/// Whether the receipt notes a service-side rewrite (§9.3 informational note).
pub(super) fn rewrite_noted(transcript: &Transcript) -> bool {
    status_of(transcript, "receipt-note") == Some(Status::Info)
}

fn status_of(transcript: &Transcript, id: &str) -> Option<Status> {
    transcript
        .checks
        .iter()
        .find(|c| c.def.id == id)
        .map(|c| c.status)
}

/// Default console reporter: one line per request; loud on verification
/// failure; keep serving either way.
pub(super) fn default_reporter(outcome: RequestOutcome) {
    let tag = if outcome.streamed { " (streamed)" } else { "" };
    let mut line = format!(
        "{} {} -> {}{tag}",
        outcome.method, outcome.path, outcome.status
    );
    if !outcome.detail.is_empty() {
        line.push_str(" — ");
        line.push_str(&outcome.detail);
    }
    if outcome.verified == Some(false) {
        tracing::warn!("!! {line}");
    } else {
        println!("{line}");
    }
}

pub(super) fn json_reporter(outcome: RequestOutcome) {
    let event = request_outcome_event(outcome);
    if let Err(error) = write_json_event(&event) {
        tracing::warn!("private-ai-proxy serve: cannot write JSON event: {error}");
    }
}

pub(super) fn json_event_sink(event: VerifierEvent) {
    let value = match event {
        VerifierEvent::IdentityUpdated { identity } => {
            lifecycle_json("identity_updated", identity, None)
        }
        VerifierEvent::Blocked {
            code: Some(code),
            reason,
        } => json!({"type": "blocked", "code": code, "reason": reason}),
        VerifierEvent::Blocked { code: None, reason } => {
            json!({"type": "blocked", "reason": reason})
        }
        VerifierEvent::Fatal { message } => json!({"type": "fatal", "message": message}),
        VerifierEvent::Terminated { error } => {
            json!({"type": "terminated", "error": error})
        }
        VerifierEvent::Ready { .. } => return,
    };
    if let Err(error) = write_json_event(&value) {
        tracing::warn!("private-ai-proxy serve: cannot write JSON event: {error}");
    }
}

pub(super) fn identity_event(
    report: &AttestationReport,
    identity: &EstablishedIdentity,
    observed_spki: Option<&str>,
    verification: Value,
) -> IdentityEvent {
    IdentityEvent {
        trust_level: "hardware_verified".to_string(),
        tee_type: report.attestation.tee_type.clone(),
        keyset_digest: report.workload_keyset_digest.clone(),
        keyset_not_after: identity.keyset.not_after,
        tls_spki: observed_spki.map(str::to_string),
        source_provenance: IdentitySourceProvenance {
            repo_url: report.attestation.source_provenance.repo_url.clone(),
            repo_commit: report.attestation.source_provenance.repo_commit.clone(),
            image_digest: report.attestation.source_provenance.image_digest.clone(),
        },
        service_capabilities: report.service_capabilities.clone(),
        verification,
    }
}

pub(super) fn request_outcome_event(outcome: RequestOutcome) -> Value {
    json!({
        "type": "request_complete",
        "method": outcome.method.as_str(),
        "path": outcome.path,
        "status": outcome.status,
        "streamed": outcome.streamed,
        "receipt_id": outcome.receipt_id,
        "verified": outcome.verified,
        "detail": outcome.detail,
        "rewritten": outcome.rewritten,
        "local_policy_applied": outcome.local_policy_applied,
    })
}

pub(super) fn write_json_event(event: &impl serde::Serialize) -> Result<(), String> {
    let line = serde_json::to_string(event)
        .map_err(|error| format!("failed to serialize serve event: {error}"))?;
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    writeln!(writer, "{line}").map_err(|error| format!("failed to write serve event: {error}"))?;
    writer
        .flush()
        .map_err(|error| format!("failed to flush serve event: {error}"))
}

pub(super) fn lifecycle_json(kind: &str, identity: IdentityEvent, extra: Option<Value>) -> Value {
    let mut object = serde_json::to_value(identity)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    object.insert("type".to_string(), Value::String(kind.to_string()));
    if let Some(Value::Object(extra)) = extra {
        object.extend(extra);
    }
    Value::Object(object)
}
