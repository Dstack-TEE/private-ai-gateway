//! Independently verify ACI artifacts the way a relying party would, following
//! the layered verification procedure in `spec/aci.md` §10. This is a practical,
//! readable reference — not a hardened SDK — that shows how the report, receipt,
//! and attested session fit together.
//!
//! What each level checks here:
//!
//! * **Level 2 — establish identity (§10.1).** The report's `workload_id`,
//!   `workload_keyset_digest`, and `report_data` binding plus the keyset
//!   endorsement, via [`validate_aci_report_binding`]. Verifying the raw
//!   hardware quote against a TEE vendor root needs a platform DCAP verifier and
//!   is out of scope for this example; everything downstream is checked against
//!   the keyset this step establishes.
//! * **Level 1 — verify the inference (§10.2).** The receipt signature under an
//!   attested `receipt_signing_keys` entry, the receipt's workload binding, and
//!   the request/response body hashes against bytes you supply. Transparency
//!   events are cross-checked: a rewritten `request.forwarded` must be flagged by
//!   `transparency.request_modified`.
//! * **Level 3 — deep audit (§10.3).** Given an attested session record,
//!   recompute its content-addressed `session_id` (§9.2, including the
//!   null-restoration rule), check that `evidence.data` hashes to
//!   `evidence.digest`, and confirm the receipt's `upstream.verified` event
//!   commits to that same session.
//!
//! The receipt and session are handled as raw JSON: a verifier checks the
//! signature over the canonical bytes it actually received (§8.5), so there is
//! no need to reproject them through typed structs first.
//!
//! Usage:
//!
//! ```text
//! cargo run --example verify_aci_artifacts -- \
//!   --report report.json --receipt receipt.json \
//!   [--nonce <value>] [--request-body request.json] [--response-body response.json] \
//!   [--session session.json] [--skip-freshness]
//! ```

use std::env;
use std::fs;
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use private_ai_gateway::aci::canonical::{canonicalize, jcs_sha256_hex, sha256_hex};
use private_ai_gateway::aci::keys::verify_receipt_signature;
use private_ai_gateway::aci::types::AttestationReport;
use private_ai_gateway::aci::verifier::validate_aci_report_binding;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Default)]
struct Args {
    report_path: String,
    receipt_path: String,
    session_path: Option<String>,
    nonce: Option<String>,
    request_body_path: Option<String>,
    response_body_path: Option<String>,
    skip_freshness: bool,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let report_bytes = fs::read(&args.report_path)
        .map_err(|e| format!("failed to read report {}: {e}", args.report_path))?;
    let report: AttestationReport = serde_json::from_slice(&report_bytes)
        .map_err(|e| format!("failed to parse report JSON: {e}"))?;
    // Accept the bare canonical receipt or a `{ "receipt": .. }` wrapper.
    let raw = read_json_file(&args.receipt_path)?;
    let receipt = raw.get("receipt").cloned().unwrap_or(raw);

    // --- Level 2: establish the workload identity (§10.1) ---
    let now_secs = if args.skip_freshness {
        report.attestation.freshness.fetched_at
    } else {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system time is before UNIX_EPOCH: {e}"))?
            .as_secs()
    };
    let validated = validate_aci_report_binding(
        &report,
        args.nonce.as_deref(),
        now_secs,
        Some(&report_bytes),
    )
    .map_err(|e| format!("ACI report binding failed: {e}"))?;

    // --- Level 1: verify the inference against that identity (§10.2) ---
    // The receipt binds back to the established identity (§8.5) rather than
    // carrying its own attestation.
    let identity_match = field_str(&receipt, "workload_id") == Some(validated.workload_id.as_str())
        && field_str(&receipt, "workload_keyset_digest")
            == Some(validated.workload_keyset_digest.as_str());
    let receipt_signature_valid = verify_receipt(&receipt, &report)?;

    // The request hash covers the body the service observed after TLS / E2EE
    // termination (§8.3); for E2EE requests that is the decrypted body.
    let request_hash_valid = args
        .request_body_path
        .as_deref()
        .map(|path| compare_body_hash(path, &receipt, "request.received", "body_hash"))
        .transpose()?;
    // For a plaintext response the client's bytes match `wire_hash`; for an E2EE
    // response the decrypted bytes match `cleartext_hash` instead (§10.2 step 4).
    let response_hash_valid = match &args.response_body_path {
        Some(path) => {
            let body =
                fs::read(path).map_err(|e| format!("failed to read response body {path}: {e}"))?;
            let expected = sha256_hex(&body);
            let event = event_by_type(&receipt, "response.returned")
                .ok_or_else(|| "receipt missing response.returned event".to_string())?;
            Some(
                field_str(event, "cleartext_hash") == Some(expected.as_str())
                    || field_str(event, "wire_hash") == Some(expected.as_str()),
            )
        }
        None => None,
    };
    // A `request.forwarded.body_hash` that differs from `request.received` MUST
    // be accompanied by `transparency.request_modified` (§10.2 step 5).
    let transparency_consistent = transparency_consistent(&receipt);

    // --- Level 3: audit the upstream (§10.3) ---
    // Shallow audit: surface each `upstream.verified` event and its typed claims;
    // local policy would decide what to require (e.g. `tee_attested` asserted
    // from `hardware_proven`).
    let upstream_events = events(&receipt)
        .filter(|event| field_str(event, "type") == Some("upstream.verified"))
        .map(|event| {
            json!({
                "seq": event.get("seq"),
                "result": field_str(event, "result"),
                "upstream_name": field_str(event, "upstream_name"),
                "provider_type": field_str(event, "provider_type"),
                "model_id": field_str(event, "model_id"),
                "url_origin": field_str(event, "url_origin"),
                "verifier_id": field_str(event, "verifier_id"),
                "required": event.get("required").and_then(Value::as_bool),
                "reason": field_str(event, "reason"),
                "session_id": field_str(event, "session_id"),
                "channel_binding_count": event
                    .get("channel_bindings")
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len),
                "claims": event.get("claims").cloned().unwrap_or(Value::Null),
            })
        })
        .collect::<Vec<_>>();
    let transparency_events = events(&receipt)
        .filter(|event| field_str(event, "type").is_some_and(|t| t.starts_with("transparency.")))
        .map(|event| json!({ "seq": event.get("seq"), "type": field_str(event, "type") }))
        .collect::<Vec<_>>();

    // Deep audit: recompute the session id and check the evidence digest.
    let session_audit = match &args.session_path {
        Some(path) => Some(audit_session(&read_json_file(path)?, &receipt)?),
        None => None,
    };

    let verified = identity_match
        && receipt_signature_valid
        && request_hash_valid.unwrap_or(true)
        && response_hash_valid.unwrap_or(true)
        && transparency_consistent
        && session_audit.as_ref().is_none_or(SessionAudit::is_ok);

    let summary = json!({
        "verified": verified,
        "report_binding_valid": true,
        "identity_match": identity_match,
        "receipt_signature_valid": receipt_signature_valid,
        "request_hash_valid": request_hash_valid,
        "response_hash_valid": response_hash_valid,
        "transparency_consistent": transparency_consistent,
        "workload_id": validated.workload_id,
        "workload_keyset_digest": validated.workload_keyset_digest,
        "receipt_id": field_str(&receipt, "receipt_id"),
        "chat_id": receipt.get("chat_id").cloned().unwrap_or(Value::Null),
        "requested_model": receipt.get("model").cloned().unwrap_or(Value::Null),
        "upstream_events": upstream_events,
        "transparency_events": transparency_events,
        "session_audit": session_audit,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary)
            .map_err(|e| format!("failed to serialize verification summary: {e}"))?
    );
    if verified {
        Ok(())
    } else {
        Err("ACI artifact verification failed".to_string())
    }
}

/// Verify the receipt signature under an attested `receipt_signing_keys` entry.
/// The signed bytes are the JCS of the whole receipt with only `signature.value`
/// removed (§8.5) — canonicalize what was received rather than a re-projection.
fn verify_receipt(receipt: &Value, report: &AttestationReport) -> Result<bool, String> {
    let signature = receipt
        .get("signature")
        .ok_or_else(|| "receipt missing signature".to_string())?;
    let key_id = signature
        .get("key_id")
        .and_then(Value::as_str)
        .ok_or_else(|| "receipt signature missing key_id".to_string())?;
    let receipt_key = report
        .attestation
        .workload_keyset
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == key_id)
        .ok_or_else(|| {
            format!("receipt signature key_id {key_id:?} is not in the attested keyset")
        })?;
    let value_hex = signature
        .get("value")
        .and_then(Value::as_str)
        .ok_or_else(|| "receipt signature missing value".to_string())?;
    let sig = hex::decode(value_hex).map_err(|e| format!("invalid receipt signature hex: {e}"))?;

    let mut unsigned = receipt.clone();
    if let Some(obj) = unsigned.get_mut("signature").and_then(Value::as_object_mut) {
        obj.remove("value");
    }
    let canonical =
        canonicalize(&unsigned).map_err(|e| format!("failed to canonicalize receipt: {e}"))?;
    Ok(verify_receipt_signature(receipt_key, &canonical, &sig))
}

/// Deep-audit outcome for one attested-session record (§10.3 step 3).
#[derive(Serialize)]
struct SessionAudit {
    session_id: String,
    recomputed_session_id: String,
    /// The record's `session_id` equals the id recomputed from its material.
    session_id_matches: bool,
    /// `evidence.data` decodes and hashes to `evidence.digest`.
    evidence_digest_valid: bool,
    /// Some `upstream.verified` event in the receipt commits to this session id.
    referenced_by_receipt: bool,
}

impl SessionAudit {
    fn is_ok(&self) -> bool {
        self.session_id_matches && self.evidence_digest_valid && self.referenced_by_receipt
    }
}

/// Recompute the content-addressed `session_id` from a fetched record and check
/// its evidence, then confirm the receipt points at it (§9.2, §10.3).
fn audit_session(record: &Value, receipt: &Value) -> Result<SessionAudit, String> {
    let session_id = field_str(record, "session_id")
        .ok_or_else(|| "session record missing string session_id".to_string())?
        .to_string();
    let recomputed_session_id = recompute_session_id(record)?;
    let referenced_by_receipt = events(receipt)
        .filter(|event| field_str(event, "type") == Some("upstream.verified"))
        .any(|event| field_str(event, "session_id") == Some(session_id.as_str()));
    Ok(SessionAudit {
        session_id_matches: recomputed_session_id == session_id,
        session_id,
        recomputed_session_id,
        evidence_digest_valid: evidence_digest_matches(record.get("evidence")),
        referenced_by_receipt,
    })
}

/// `session_id = "as_" || hex(sha256(JCS(material)))` over the immutable subset
/// (§9.2). Timestamps and the re-fetchable evidence bytes are excluded, and the
/// three optional wire fields (`endpoint`, `identity`, `evidence.digest`) are
/// restored to JSON `null` when absent — the null-restoration rule.
fn recompute_session_id(record: &Value) -> Result<String, String> {
    let required = |key: &str| {
        record
            .get(key)
            .cloned()
            .ok_or_else(|| format!("session record missing {key:?}"))
    };
    let optional = |key: &str| record.get(key).cloned().unwrap_or(Value::Null);
    let material = json!({
        "upstream_name": required("upstream_name")?,
        "endpoint": optional("endpoint"),
        "verifier_id": required("verifier_id")?,
        "identity": optional("identity"),
        "channel_binding": required("channel_binding")?,
        "claims": required("claims")?,
        "evidence_digest": record
            .get("evidence")
            .and_then(|evidence| evidence.get("digest"))
            .cloned()
            .unwrap_or(Value::Null),
    });
    let digest = jcs_sha256_hex(&material)
        .map_err(|e| format!("failed to canonicalize session material: {e}"))?;
    Ok(format!(
        "as_{}",
        digest.strip_prefix("sha256:").unwrap_or(digest.as_str())
    ))
}

/// True when there is nothing to check, or `evidence.data` decodes and hashes to
/// `evidence.digest` (§9.2: a record whose data does not match its digest MUST
/// be rejected). We only ever emit `data:<content-type>;base64,<...>`.
fn evidence_digest_matches(evidence: Option<&Value>) -> bool {
    let Some(evidence) = evidence else {
        return true;
    };
    let (Some(digest), Some(data_uri)) = (
        evidence.get("digest").and_then(Value::as_str),
        evidence.get("data").and_then(Value::as_str),
    ) else {
        return true;
    };
    let Some((_, b64)) = data_uri.split_once(";base64,") else {
        return true;
    };
    match BASE64.decode(b64.as_bytes()) {
        Ok(bytes) => sha256_hex(&bytes) == digest,
        Err(_) => false,
    }
}

/// §10.2 step 5: any `request.forwarded` that differs from `request.received`
/// must be flagged with `transparency.request_modified`.
fn transparency_consistent(receipt: &Value) -> bool {
    let hash =
        |event_type| event_by_type(receipt, event_type).and_then(|e| field_str(e, "body_hash"));
    let rewritten = match (hash("request.received"), hash("request.forwarded")) {
        (Some(received), Some(forwarded)) => received != forwarded,
        _ => false,
    };
    // The flag is required when rewritten; a flag without a rewrite is allowed.
    !rewritten || event_by_type(receipt, "transparency.request_modified").is_some()
}

/// Iterate a receipt's `event_log` (empty when absent or malformed).
fn events(receipt: &Value) -> impl Iterator<Item = &Value> {
    receipt
        .get("event_log")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn event_by_type<'a>(receipt: &'a Value, event_type: &str) -> Option<&'a Value> {
    events(receipt).find(|event| field_str(event, "type") == Some(event_type))
}

fn field_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn compare_body_hash(
    path: &str,
    receipt: &Value,
    event_type: &str,
    field: &str,
) -> Result<bool, String> {
    let body = fs::read(path).map_err(|e| format!("failed to read body {path}: {e}"))?;
    let expected = sha256_hex(&body);
    let event = event_by_type(receipt, event_type)
        .ok_or_else(|| format!("receipt missing {event_type} event"))?;
    Ok(field_str(event, field) == Some(expected.as_str()))
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut iter = env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--report" => args.report_path = next_arg(&mut iter, "--report")?,
            "--receipt" => args.receipt_path = next_arg(&mut iter, "--receipt")?,
            "--session" => args.session_path = Some(next_arg(&mut iter, "--session")?),
            "--nonce" => args.nonce = Some(next_arg(&mut iter, "--nonce")?),
            "--request-body" => {
                args.request_body_path = Some(next_arg(&mut iter, "--request-body")?)
            }
            "--response-body" => {
                args.response_body_path = Some(next_arg(&mut iter, "--response-body")?)
            }
            "--skip-freshness" => args.skip_freshness = true,
            "--help" | "-h" => {
                println!(
                    "usage: cargo run --example verify_aci_artifacts -- \
                     --report report.json --receipt receipt.json [--nonce value] \
                     [--request-body request.json] [--response-body response.json] \
                     [--session session.json] [--skip-freshness]"
                );
                process::exit(0);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if args.report_path.is_empty() {
        return Err("--report is required".to_string());
    }
    if args.receipt_path.is_empty() {
        return Err("--receipt is required".to_string());
    }
    Ok(args)
}

fn next_arg(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn read_json_file(path: &str) -> Result<Value, String> {
    let bytes = fs::read(path).map_err(|e| format!("failed to read {path}: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("failed to parse JSON {path}: {e}"))
}
