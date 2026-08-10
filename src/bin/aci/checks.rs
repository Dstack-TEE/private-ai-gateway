//! The check engine shared by every `aci` subcommand.
//!
//! Implements the spec 9.1 identity checks over an attestation report
//! (id-1..id-6), the 9.3 receipt checks (receipt-1..receipt-4), and the
//! upstream audit (upstream-1, upstream-2), per `spec/aci.md`.
//!
//! Subcommands differ only in where the artifacts come from — fetched live
//! (`verify`, `chat`, `serve`) or read from files (`audit`) — which the
//! contexts here express: quote collateral online vs offline, TLS channel
//! observed vs not, bodies supplied vs absent.
//!
//! Artifacts are canonicalized, then verified (Appendix A): the keyset
//! digest, the receipt signature and the session id are all over the JCS
//! form of the parsed document, so the served encoding is free. The
//! binding checks recompute the same §9.1 chain the lib's
//! `validate_aci_report_binding` composes, step by step, so every check gets
//! its own honest status instead of stopping at the first failure.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use private_ai_gateway::aci::digest::{jcs_bytes, sha256_hex, sha256_raw};
use private_ai_gateway::aci::identity;
use private_ai_gateway::aci::keys::verify_receipt_signature;
use private_ai_gateway::aci::receipt::receipt_signing_input;
use private_ai_gateway::aci::types::{AttestationReport, WorkloadKeyset};
use private_ai_gateway::aci::verifier::{
    appraise_report, AppraisalInputs, CheckId, CheckResult, CustodyEvidence, Outcome,
};
pub use private_ai_gateway::aci::verifier::{ChannelEvidence, QuoteSource};
use serde_json::Value;

use crate::client::{AciClient, HttpResult};
use crate::transcript::{
    Transcript, ID_1, ID_2, ID_3, ID_4, ID_5, ID_6, RECEIPT_1, RECEIPT_2, RECEIPT_3, RECEIPT_4,
    RECEIPT_NOTE, UPSTREAM_1, UPSTREAM_2,
};

pub struct ReportCheckContext<'a> {
    /// The nonce this verifier supplied on the report fetch (§3.2).
    pub nonce: Option<&'a str>,
    pub now_secs: u64,
    /// Audit `--skip-expiry` (§3.4 archival policy): id-3 is skipped, never
    /// passed.
    pub expiry_skipped: bool,
    pub quote: QuoteSource<'a>,
    pub channel: ChannelEvidence<'a>,
    /// Verifier policy (§1.3): compose hashes this caller accepts. Empty
    /// means the measurement is verified and reported, not pinned — the
    /// operator appraises the provenance themselves.
    pub accepted_composes: &'a [String],
    pub explain: bool,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The workload identity a verified report establishes (§9.1): the keyset
/// parsed from the served `workload_keyset` object, and the digest
/// recomputed over its JCS form.
pub struct EstablishedIdentity {
    pub keyset: WorkloadKeyset,
    pub keyset_digest: String,
}

/// Map one §9.1 outcome onto its transcript line.
fn render(transcript: &mut Transcript, result: &CheckResult) {
    let def = match result.id {
        CheckId::Quote => ID_1,
        CheckId::Binding => ID_2,
        CheckId::Expiry => ID_3,
        CheckId::Provenance => ID_4,
        CheckId::Custody => ID_5,
        CheckId::Channel => ID_6,
    };
    match &result.outcome {
        Outcome::Pass => transcript.pass(def, result.detail.clone()),
        Outcome::Failed(_) => transcript.fail(def, result.detail.clone()),
        // An unevaluable check is never a pass: the transcript says so, and
        // the verdict counts it as unverified.
        Outcome::Unevaluable(why) => transcript.skip(def, result.detail.clone(), why.clone()),
    }
    if let Some(explain) = &result.explain {
        transcript.explain(explain.clone());
    }
}

/// Digest + parse the report's keyset, without a transcript. The
/// subcommands use this to pick keys after the transcript reached VERIFIED.
pub fn established_identity(report: &AttestationReport) -> Result<EstablishedIdentity, String> {
    let value = &report.attestation.workload_keyset;
    let keyset_digest = identity::workload_keyset_digest(value)
        .map_err(|e| format!("workload keyset violates the ACI document constraints: {e}"))?;
    let keyset: WorkloadKeyset = serde_json::from_value(value.clone())
        .map_err(|e| format!("workload keyset does not parse: {e}"))?;
    Ok(EstablishedIdentity {
        keyset,
        keyset_digest,
    })
}

/// Run the id-1–id-6 checks over a parsed report, appending to `transcript`.
///
/// Returns `Err` only for protocol-gate problems (the report is not an
/// `aci/1` report at all); check failures land in the transcript.
pub async fn run_report_checks(
    transcript: &mut Transcript,
    report: &AttestationReport,
    cx: ReportCheckContext<'_>,
) -> Result<Option<EstablishedIdentity>, String> {
    transcript.workload_keyset_digest = Some(report.workload_keyset_digest.clone());
    // The checks, their order, and what each outcome means are the shared
    // appraisal's (`aci::verifier`); this renders the outcomes.
    let appraisal = appraise_report(AppraisalInputs {
        report,
        nonce: cx.nonce,
        now_secs: cx.now_secs,
        expiry_waived: cx.expiry_skipped,
        quote: cx.quote,
        accepted_composes: cx.accepted_composes,
        // §9.1(5) needs a custody policy this CLI does not implement yet
        // (docs/reviews/aci-spec-conformance-gaps.md item 1).
        custody: CustodyEvidence::Unimplemented {
            reason: "custody policy not implemented in this CLI yet \
                     (see src/aci/verifier/dstack.rs)",
        },
        channel: cx.channel,
        explain: cx.explain,
    })
    .await?;

    for result in &appraisal.results {
        render(transcript, result);
    }
    Ok(appraisal.identity.map(|binding| EstablishedIdentity {
        keyset: binding.keyset,
        keyset_digest: binding.keyset_digest,
    }))
}

/// Parse the §7.2 receipt document and gate its `api_version` (Appendix B).
/// The signature is checked over the JCS form of what was parsed, so the
/// served encoding is free (Appendix A).
pub fn parse_receipt_document(payload: Value) -> Result<Value, String> {
    // Appendix B: artifacts with a foreign api_version are rejected, same as
    // the report gate in run_report_checks.
    if field_str(&payload, "api_version") != Some("aci/1") {
        return Err(format!(
            "unsupported receipt api_version {:?} (expected \"aci/1\")",
            payload.get("api_version").unwrap_or(&Value::Null)
        ));
    }
    Ok(payload)
}

/// A body's `sha256:` digest and byte count — everything receipt-3 and
/// receipt-4 compare, so a verifier can record digests instead of holding
/// bodies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodyDigest {
    /// `sha256:<hex>`, the receipt `body_hash` format (§7.4).
    pub sha256: String,
    pub len: u64,
}

impl BodyDigest {
    pub fn of(bytes: &[u8]) -> Self {
        Self::from_sha256(sha256_raw(bytes), bytes.len() as u64)
    }

    pub fn from_sha256(raw: [u8; 32], len: u64) -> Self {
        Self {
            sha256: format!("sha256:{}", hex::encode(raw)),
            len,
        }
    }
}

pub struct ReceiptContext<'a> {
    /// The parsed §7.2 receipt document.
    pub receipt: &'a Value,
    /// The established keyset (§9.1) whose `receipt_signing_keys` resolve
    /// the envelope `key_id`, plus its recomputed digest.
    pub keyset: &'a WorkloadKeyset,
    pub workload_keyset_digest: &'a str,
    /// Digest of the exact request body bytes the client sent, when available.
    pub request_body: Option<&'a BodyDigest>,
    /// Digest of the exact response bytes as read off the wire, when available.
    pub response_wire: Option<&'a BodyDigest>,
}

impl<'a> ReceiptContext<'a> {
    /// Receipt context for an established identity and the exact body digests.
    /// The subcommands render receipts without `--explain`, so it is off here.
    pub fn new(
        receipt: &'a Value,
        identity: &'a EstablishedIdentity,
        request_body: Option<&'a BodyDigest>,
        response_wire: Option<&'a BodyDigest>,
    ) -> Self {
        Self {
            receipt,
            keyset: &identity.keyset,
            workload_keyset_digest: &identity.keyset_digest,
            request_body,
            response_wire,
        }
    }
}

fn events(payload: &Value) -> impl Iterator<Item = &Value> {
    payload
        .get("event_log")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
}

fn event_by_type<'a>(payload: &'a Value, event_type: &str) -> Option<&'a Value> {
    events(payload).find(|event| field_str(event, "type") == Some(event_type))
}

fn field_str<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

/// The `session_id` the serving (verified) `upstream.verified` event commits
/// to — the handle for the §9.3 deep audit. Filtering on the verified result
/// keeps the fetch and upstream-2 on the same session upstream-1 blessed, even when §7.5
/// prior-attempt events are also present.
pub fn session_id_from_receipt(payload: &Value) -> Option<String> {
    events(payload)
        .filter(|event| {
            field_str(event, "type") == Some("upstream.verified")
                && field_str(event, "result") == Some("verified")
        })
        .find_map(|event| field_str(event, "session_id").map(str::to_string))
}

/// Fetch the attested session record the receipt commits to — the live
/// artifact source for the upstream-2 deep audit. Returns the raw 2xx response (the
/// document hashes to the session id, §8) and otherwise the reason
/// upstream-2 cites for skipping.
pub async fn fetch_live_session(
    client: &AciClient,
    base_url: &str,
    payload: &Value,
) -> (Option<HttpResult>, String) {
    let mut no_session_reason = "receipt's upstream.verified carries no session_id".to_string();
    let resp = match session_id_from_receipt(payload) {
        None => None,
        Some(session_id) => match client.fetch_session(base_url, &session_id).await {
            Ok(resp) if (200..300).contains(&resp.status) => Some(resp),
            Ok(resp) => {
                no_session_reason =
                    format!("session {session_id} fetch returned HTTP {}", resp.status);
                None
            }
            Err(e) => {
                no_session_reason = format!("session {session_id} fetch failed: {e}");
                None
            }
        },
    };
    (resp, no_session_reason)
}

/// Run receipt-1–receipt-4 (§9.3) over a receipt document against an established identity.
pub fn run_receipt_checks(transcript: &mut Transcript, cx: ReceiptContext<'_>) {
    // receipt-1 — the signature over JCS(document minus `signature`), under the
    // attested keyset entry `key_id` names; that entry decides the
    // algorithm (§7.2).
    check_signature(transcript, &cx);

    // receipt-2 — the payload binds back to the established keyset digest.
    let payload_digest = field_str(cx.receipt, "workload_keyset_digest");
    if payload_digest == Some(cx.workload_keyset_digest) {
        transcript.pass(
            RECEIPT_2,
            "payload workload_keyset_digest matches the established digest",
        );
    } else {
        transcript.fail(
            RECEIPT_2,
            format!(
                "payload carries workload_keyset_digest {payload_digest:?}, established {}",
                cx.workload_keyset_digest
            ),
        );
    }

    // receipt-3 — request.received.body_hash covers the plaintext wire bytes,
    // or the compact post-decryption JSON body for E2EE v2 (§7.4).
    match cx.request_body {
        None => transcript.skip(
            RECEIPT_3,
            "request body digest not supplied",
            "request digest not supplied",
        ),
        Some(digest) => {
            match event_by_type(cx.receipt, "request.received")
                .and_then(|event| field_str(event, "body_hash"))
            {
                None => transcript.fail(RECEIPT_3, "receipt has no request.received body_hash"),
                Some(recorded) if recorded == digest.sha256 => {
                    transcript.pass(
                        RECEIPT_3,
                        format!("{} over {} bytes", digest.sha256, digest.len),
                    );
                }
                Some(recorded) => transcript.fail(
                    RECEIPT_3,
                    format!("computed {}, receipt records {recorded}", digest.sha256),
                ),
            }
        }
    }

    // receipt-4 — response.returned covers the exact bytes read off the wire
    // (raw SSE bytes for a stream, including encrypted E2EE fields, §7.4).
    match cx.response_wire {
        None => transcript.skip(
            RECEIPT_4,
            "response wire digest not supplied",
            "response digest not supplied",
        ),
        Some(digest) => {
            match event_by_type(cx.receipt, "response.returned")
                .and_then(|event| field_str(event, "body_hash"))
            {
                None => transcript.fail(RECEIPT_4, "receipt has no response.returned body_hash"),
                Some(recorded) if recorded == digest.sha256 => {
                    transcript.pass(
                        RECEIPT_4,
                        format!("{} over {} bytes", digest.sha256, digest.len),
                    );
                }
                Some(recorded) => transcript.fail(
                    RECEIPT_4,
                    format!("computed {}, receipt records {recorded}", digest.sha256),
                ),
            }
        }
    }

    // §9.3 rewrite note: differing request.forwarded/request.received hashes
    // are the service-side rewrite. ACI records it, nothing more — whether a
    // rewrite is acceptable is local policy, so this is an info line.
    let received = event_by_type(cx.receipt, "request.received")
        .and_then(|event| field_str(event, "body_hash"));
    let forwarded = event_by_type(cx.receipt, "request.forwarded")
        .and_then(|event| field_str(event, "body_hash"));
    if let (Some(received), Some(forwarded)) = (received, forwarded) {
        if received != forwarded {
            transcript.info(
                RECEIPT_NOTE,
                format!(
                    "the service rewrote the request before inference: \
                     request.forwarded {forwarded} != request.received {received} \
                     (acceptability is local policy)"
                ),
            );
        }
    }
}

fn check_signature(transcript: &mut Transcript, cx: &ReceiptContext<'_>) {
    let Some(key_id) = field_str(cx.receipt, "key_id") else {
        transcript.fail(RECEIPT_1, "receipt document has no key_id");
        return;
    };
    let Some(receipt_key) = cx
        .keyset
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == key_id)
    else {
        transcript.fail(
            RECEIPT_1,
            format!("receipt key_id {key_id:?} is not in the attested keyset"),
        );
        return;
    };
    let Some(signature_hex) = field_str(cx.receipt, "signature") else {
        transcript.fail(RECEIPT_1, "receipt document has no signature");
        return;
    };
    let Ok(signature) = hex::decode(signature_hex) else {
        transcript.fail(RECEIPT_1, "receipt signature is not hex");
        return;
    };
    // §7.2: the signature covers JCS(document minus `signature`); the
    // attested keyset entry decides the algorithm (Appendix A).
    let signing_input = match receipt_signing_input(cx.receipt) {
        Ok(input) => input,
        Err(e) => {
            transcript.fail(
                RECEIPT_1,
                format!("receipt violates the ACI document constraints: {e}"),
            );
            return;
        }
    };
    if receipt_key.algo != "ed25519" {
        transcript.fail(
            RECEIPT_1,
            format!(
                "receipt key {key_id:?} uses algorithm {:?}, which this verifier does not \
                 implement (Appendix B rejects the artifact, not the entry)",
                receipt_key.algo
            ),
        );
        return;
    }
    if verify_receipt_signature(receipt_key, &signing_input, &signature) {
        transcript.pass(
            RECEIPT_1,
            format!(
                "{} signature by attested key {key_id:?} verifies over JCS(document minus signature)",
                receipt_key.algo
            ),
        );
    } else {
        transcript.fail(
            RECEIPT_1,
            format!("signature by key {key_id:?} does not verify"),
        );
    }
}

/// The §9.3 + §9.2 sequence every response-verifying subcommand runs: the
/// receipt checks over the established identity and observed bytes, then the
/// upstream checks over the cited session.
pub fn run_response_checks(
    transcript: &mut Transcript,
    receipt: &Value,
    identity: &EstablishedIdentity,
    request_body: Option<&BodyDigest>,
    response_wire: Option<&BodyDigest>,
    upstream: UpstreamContext<'_>,
) {
    run_receipt_checks(
        transcript,
        ReceiptContext::new(receipt, identity, request_body, response_wire),
    );
    run_upstream_checks(transcript, receipt, upstream);
}

pub struct UpstreamContext<'a> {
    pub session_bytes: Option<&'a [u8]>,
    pub no_session_reason: &'a str,
    /// The client's §5.3 pinned session ids, when any.
    pub pinned: Option<&'a [String]>,
    pub requires_verified: bool,
    /// The report's `service_capabilities.serving` (§4.1): only a `direct`
    /// service may omit `upstream.verified` (§7.5).
    pub serving: &'a str,
    /// The client's §9.2(3) claims policy: every required claim must be
    /// satisfied by the cited session, or upstream-2 fails.
    pub required_claims: &'a [RequiredClaim],
}

/// A §9.2(3) claims requirement: the named typed claim must be `asserted`,
/// optionally from an exact source (`name` or `name=source`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RequiredClaim {
    pub name: String,
    pub source: Option<String>,
}

/// The claim sources the spec defines (§8.3); Appendix B treats anything
/// else as `unknown`, so a policy naming one would never be satisfiable.
pub const CLAIM_SOURCES: &[&str] = &[
    "hardware_proven",
    "verifier_derived",
    "provider_asserted",
    "operator_asserted",
];

impl RequiredClaim {
    pub fn parse(value: &str) -> Result<Self, String> {
        let (name, source) = match value.split_once('=') {
            Some((name, source)) => (name, Some(source)),
            None => (value, None),
        };
        if name.is_empty() || name == "extra" {
            return Err(format!("{value:?} does not name a typed claim (spec 8.3)"));
        }
        if let Some(source) = source {
            if !CLAIM_SOURCES.contains(&source) {
                return Err(format!(
                    "unknown claim source {source:?} (expected one of: {})",
                    CLAIM_SOURCES.join(", ")
                ));
            }
        }
        Ok(Self {
            name: name.to_string(),
            source: source.map(str::to_string),
        })
    }
}

impl std::fmt::Display for RequiredClaim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source {
            Some(source) => write!(f, "{}={source}", self.name),
            None => write!(f, "{}", self.name),
        }
    }
}

/// The required claims this session record does not satisfy (§9.2(3)).
pub fn unmet_claims(record: &Value, required: &[RequiredClaim]) -> Vec<String> {
    required
        .iter()
        .filter(|req| {
            let claim = record
                .get("claims")
                .and_then(|claims| claims.get(&req.name));
            let asserted = claim.and_then(|claim| field_str(claim, "status")) == Some("asserted");
            let source_ok = match &req.source {
                None => true,
                Some(source) => {
                    claim.and_then(|claim| field_str(claim, "source")) == Some(source.as_str())
                }
            };
            !(asserted && source_ok)
        })
        .map(RequiredClaim::to_string)
        .collect()
}

/// The §9.2(1)-(2) integrity audit of one served session record.
pub struct SessionAudit {
    pub record: Value,
    pub recomputed_id: String,
    pub id_matches: bool,
    pub version_ok: bool,
    pub in_window: bool,
    pub evidence: Result<(), String>,
}

impl SessionAudit {
    pub fn integrity_ok(&self) -> bool {
        self.version_ok && self.id_matches && self.in_window && self.evidence.is_ok()
    }
}

/// Audit one served session record: the parsed document's JCS form hashes to
/// `expected_id` (§8), the validity window contains `at` (a receipt's
/// `served_at`, or now for a live listing), and the evidence data hashes to
/// its digest (§8.2). `Err` is a record that cannot be audited at all.
pub fn audit_session_record(
    bytes: &[u8],
    expected_id: &str,
    at: Option<u64>,
) -> Result<SessionAudit, String> {
    let record: Value =
        serde_json::from_slice(bytes).map_err(|e| format!("session record is not JSON: {e}"))?;
    // §8: the id is the hash of the JCS form of the parsed document, so any
    // served encoding of the same content matches.
    let recomputed_id = jcs_bytes(&record)
        .map(|bytes| hex::encode(sha256_raw(&bytes)))
        .map_err(|e| format!("session record violates the ACI document constraints: {e}"))?;
    // Appendix B: a session document with a foreign api_version is rejected.
    let version_ok = field_str(&record, "api_version") == Some("aci/1");
    let id_matches = recomputed_id == expected_id;
    let window = (
        record.get("established_at").and_then(Value::as_u64),
        record.get("expires_at").and_then(Value::as_u64),
    );
    let in_window = match (at, window) {
        (Some(at), (Some(from), Some(until))) => from <= at && at <= until,
        _ => false,
    };
    let evidence = evidence_check(record.get("evidence"));
    Ok(SessionAudit {
        record,
        recomputed_id,
        id_matches,
        version_ok,
        in_window,
        evidence,
    })
}

pub fn run_upstream_checks(transcript: &mut Transcript, payload: &Value, cx: UpstreamContext<'_>) {
    check_upstream_event(transcript, payload, cx.serving, cx.requires_verified);
    check_session_audit(
        transcript,
        payload,
        cx.session_bytes,
        cx.no_session_reason,
        cx.pinned,
        cx.requires_verified,
        cx.required_claims,
    );
}

/// upstream-1 — the serving upstream was verified before the prompt was forwarded
/// and the event cites the session holding the verification detail. §7.5
/// allows prior failed attempts alongside the verified one.
fn check_upstream_event(
    transcript: &mut Transcript,
    payload: &Value,
    serving: &str,
    requires_verified: bool,
) {
    let upstream_events: Vec<&Value> = events(payload)
        .filter(|event| field_str(event, "type") == Some("upstream.verified"))
        .collect();
    let verified = upstream_events
        .iter()
        .find(|event| field_str(event, "result") == Some("verified"));
    match verified {
        // §5.3: a direct service satisfies verified serving by construction —
        // the workload verified in §9.1 is the one serving, with no second
        // hop to attest.
        _ if upstream_events.is_empty() && serving == "direct" => transcript.pass(
            UPSTREAM_1,
            "direct service (spec 4.1): no upstream hop; the spec 9.1-verified workload serves",
        ),
        // §7.5: every aggregator receipt records the event. A missing event
        // on a non-direct service is a conformance failure, not an absence.
        _ if upstream_events.is_empty() => transcript.fail(
            UPSTREAM_1,
            format!(
                "receipt carries no upstream.verified event, and serving={serving:?} is not \
                 \"direct\" (spec 7.5)"
            ),
        ),
        // §9.3(5) rejects unverified serving only for a client that requires
        // it; otherwise the receipt's own record is the answer.
        None if requires_verified => transcript.fail(
            UPSTREAM_1,
            "no upstream.verified event reports a verified upstream",
        ),
        None => transcript.info(UPSTREAM_1, "the receipt records unverified serving"),
        Some(event)
            if requires_verified
                && event.get("required").and_then(Value::as_bool) != Some(true) =>
        {
            transcript.fail(UPSTREAM_1, "verified upstream but required is not true")
        }
        Some(event) => match field_str(event, "session_id") {
            None => transcript.fail(UPSTREAM_1, "verified upstream but cites no session_id"),
            Some(session) => transcript.pass(
                UPSTREAM_1,
                format!(
                    "model={} session={session} ({} attempt(s))",
                    field_str(event, "model_id").unwrap_or("?"),
                    upstream_events.len()
                ),
            ),
        },
    }
}

/// upstream-2 — deep audit of the attested session record: the parsed document's
/// JCS form hashes to the cited id (§8), the receipt's served_at falls in
/// the session's validity window, and the evidence data hashes to its
/// digest.
fn check_session_audit(
    transcript: &mut Transcript,
    payload: &Value,
    session_bytes: Option<&[u8]>,
    no_session_reason: &str,
    pinned: Option<&[String]>,
    requires_verified: bool,
    required_claims: &[RequiredClaim],
) {
    // §9.3(6) membership, when the client pinned sessions (§5.3): the cited
    // id must be one the client listed, whether or not the record fetch
    // succeeded.
    if let (Some(pinned), Some(cited)) = (pinned, session_id_from_receipt(payload)) {
        if !pinned.contains(&cited) {
            transcript.fail(
                UPSTREAM_2,
                format!("cited session {cited} is not in the pinned list (spec 5.3)"),
            );
            return;
        }
    }

    let Some(bytes) = session_bytes else {
        // §8 retention: a cited session must stay resolvable for as long as
        // the receipt citing it. A client demanding verified serving cannot
        // accept "the record was there, trust us".
        if requires_verified && session_id_from_receipt(payload).is_some() {
            transcript.fail(
                UPSTREAM_2,
                format!(
                    "the cited session could not be audited: {no_session_reason} (spec 8 retention)"
                ),
            );
        } else {
            transcript.skip(UPSTREAM_2, no_session_reason, "no session record");
        }
        return;
    };
    let Some(cited) = session_id_from_receipt(payload) else {
        transcript.fail(UPSTREAM_2, "receipt cites no session_id to audit against");
        return;
    };
    let served_at = payload.get("served_at").and_then(Value::as_u64);
    let audit = match audit_session_record(bytes, &cited, served_at) {
        Ok(audit) => audit,
        Err(e) => {
            transcript.fail(UPSTREAM_2, e);
            return;
        }
    };
    let unmet = unmet_claims(&audit.record, required_claims);
    let clause = |ok: bool, yes: &str, no: &str| if ok { yes.to_string() } else { no.to_string() };
    let mut detail = format!(
        "session {cited}: {}; {}; {}; claims: {}",
        clause(
            audit.id_matches,
            "document hashes to the cited id",
            &format!(
                "document hashes to {}, NOT the cited id",
                audit.recomputed_id
            )
        ),
        clause(
            audit.in_window,
            "receipt served_at inside the validity window",
            "receipt served_at OUTSIDE the validity window"
        ),
        match &audit.evidence {
            Ok(()) => "evidence data hashes to its digest".to_string(),
            Err(reason) => reason.clone(),
        },
        claims_summary(audit.record.get("claims")),
    );
    if !audit.version_ok {
        detail = format!(
            "record api_version {:?} is not \"aci/1\"; {detail}",
            audit.record.get("api_version").unwrap_or(&Value::Null)
        );
    }
    if !unmet.is_empty() {
        detail.push_str(&format!(
            "; required claims unmet (spec 9.2(3)): {}",
            unmet.join(", ")
        ));
    }
    if audit.integrity_ok() && unmet.is_empty() {
        transcript.pass(UPSTREAM_2, detail);
    } else {
        transcript.fail(UPSTREAM_2, detail);
    }
}

/// Typed-claims one-liner for the upstream-2 detail (shallow audit surface, §9.2(3)).
fn claims_summary(claims: Option<&Value>) -> String {
    let Some(map) = claims.and_then(Value::as_object) else {
        return "none recorded".to_string();
    };
    let mut parts: Vec<String> = map
        .iter()
        .filter(|(name, _)| name.as_str() != "extra")
        .map(|(name, claim)| {
            let status = field_str(claim, "status").unwrap_or("?");
            match field_str(claim, "source") {
                Some(source) => format!("{name}={status}({source})"),
                None => format!("{name}={status}"),
            }
        })
        .collect();
    if parts.is_empty() {
        parts.push("none recorded".to_string());
    }
    parts.join(", ")
}

/// §9.2(2): `evidence.data` decodes and hashes to `evidence.digest` (§8.2:
/// a record whose data does not match its digest MUST be rejected). Missing
/// or malformed evidence rejects too — the deep audit never assumes.
fn evidence_check(evidence: Option<&Value>) -> Result<(), String> {
    let (Some(digest), Some(data_uri)) = (
        evidence.and_then(|e| field_str(e, "digest")),
        evidence.and_then(|e| field_str(e, "data")),
    ) else {
        return Err("record carries no spec 8.2 evidence digest+data".to_string());
    };
    let Some((_, b64)) = data_uri.split_once(";base64,") else {
        return Err("evidence data is not a base64 data URI".to_string());
    };
    let bytes = BASE64
        .decode(b64.as_bytes())
        .map_err(|e| format!("evidence data does not decode: {e}"))?;
    if sha256_hex(&bytes) == digest {
        Ok(())
    } else {
        Err("evidence data DOES NOT hash to its digest".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_fixtures::{
        vector_receipt_envelope, vector_receipt_envelope_rewritten, vector_report,
        vector_session_bytes, KEYSET_NOT_AFTER, REQUEST_BODY, RESPONSE_BODY, SERVED_AT, TEST_NONCE,
    };
    use crate::transcript::Status;
    use private_ai_gateway::aci::verifier::validate_aci_report_binding;

    fn offline_cx<'a>(nonce: Option<&'a str>, now_secs: u64) -> ReportCheckContext<'a> {
        ReportCheckContext {
            nonce,
            now_secs,
            expiry_skipped: false,
            quote: QuoteSource::Offline {
                reason: "quote collateral offline",
            },
            accepted_composes: &[],
            channel: ChannelEvidence::Unobservable {
                reason: "offline audit: no live TLS channel observed",
            },
            explain: false,
        }
    }

    fn status_of(t: &Transcript, id: &str) -> Status {
        t.checks
            .iter()
            .find(|c| c.def.id == id)
            .unwrap_or_else(|| panic!("check {id} missing from transcript"))
            .status
    }

    #[tokio::test]
    async fn fixture_report_binding_checks_pass_and_agree_with_lib_validator() {
        let report = vector_report();
        let now = SERVED_AT;
        let mut t = Transcript::default();
        run_report_checks(&mut t, &report, offline_cx(Some(TEST_NONCE), now))
            .await
            .unwrap();

        assert_eq!(status_of(&t, "id-2"), Status::Pass);
        assert_eq!(status_of(&t, "id-3"), Status::Pass);
        // The fixture report carries no hardware quote and no provenance, so
        // id-1 and id-4 both fail closed (§4.1); nothing here may pass.
        assert_eq!(status_of(&t, "id-1"), Status::Fail);
        assert_eq!(status_of(&t, "id-4"), Status::Fail);
        assert_eq!(status_of(&t, "id-5"), Status::Skip);
        assert_eq!(status_of(&t, "id-6"), Status::Skip);
        assert!(!t.verified());
        assert_eq!(
            t.workload_keyset_digest.as_deref(),
            Some(report.workload_keyset_digest.as_str())
        );

        // Both now fold the same chain, so this pins the rest of the gate:
        // the folded validator's expiry and role-separation steps accept the
        // fixture the transcript just passed.
        validate_aci_report_binding(&report, Some(TEST_NONCE), now, None).unwrap();
    }

    #[tokio::test]
    async fn tampered_keyset_digest_fails_i_2() {
        let mut report = vector_report();
        report.workload_keyset_digest =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string();
        let mut t = Transcript::default();
        run_report_checks(&mut t, &report, offline_cx(Some(TEST_NONCE), SERVED_AT))
            .await
            .unwrap();
        assert_eq!(status_of(&t, "id-2"), Status::Fail);
        assert!(validate_aci_report_binding(&report, Some(TEST_NONCE), SERVED_AT, None).is_err());
    }

    #[tokio::test]
    async fn wrong_nonce_fails_i_2() {
        let report = vector_report();
        let mut t = Transcript::default();
        run_report_checks(
            &mut t,
            &report,
            offline_cx(Some("some-other-nonce"), SERVED_AT),
        )
        .await
        .unwrap();
        assert_eq!(status_of(&t, "id-2"), Status::Fail);
        assert!(
            validate_aci_report_binding(&report, Some("some-other-nonce"), SERVED_AT, None)
                .is_err()
        );
    }

    #[tokio::test]
    async fn expired_keyset_fails_i_3_and_skip_expiry_skips_it() {
        let report = vector_report();
        let after_expiry = KEYSET_NOT_AFTER + 1;
        let mut t = Transcript::default();
        run_report_checks(&mut t, &report, offline_cx(Some(TEST_NONCE), after_expiry))
            .await
            .unwrap();
        assert_eq!(status_of(&t, "id-3"), Status::Fail);

        let mut t = Transcript::default();
        let mut cx = offline_cx(Some(TEST_NONCE), after_expiry);
        cx.expiry_skipped = true;
        run_report_checks(&mut t, &report, cx).await.unwrap();
        assert_eq!(status_of(&t, "id-3"), Status::Skip);
    }

    #[tokio::test]
    async fn channel_binding_matches_domain_scoped_entry() {
        let report = vector_report();
        let identity = established_identity(&report).unwrap();
        let spki = identity.keyset.tls_public_keys[0].spki_sha256_hex.clone();
        let domain = identity.keyset.tls_public_keys[0].domain.clone().unwrap();

        let mut t = Transcript::default();
        let mut cx = offline_cx(Some(TEST_NONCE), SERVED_AT);
        cx.channel = ChannelEvidence::Observed {
            host: &domain,
            spki_sha256: &spki,
        };
        run_report_checks(&mut t, &report, cx).await.unwrap();
        assert_eq!(status_of(&t, "id-6"), Status::Pass);

        // Same SPKI presented for a hostname the keyset does not scope it to.
        let mut t = Transcript::default();
        let mut cx = offline_cx(Some(TEST_NONCE), SERVED_AT);
        cx.channel = ChannelEvidence::Observed {
            host: "other.example.com",
            spki_sha256: &spki,
        };
        run_report_checks(&mut t, &report, cx).await.unwrap();
        assert_eq!(status_of(&t, "id-6"), Status::Fail);
    }

    static REQUEST_DIGEST: std::sync::LazyLock<BodyDigest> =
        std::sync::LazyLock::new(|| BodyDigest::of(REQUEST_BODY));
    static RESPONSE_DIGEST: std::sync::LazyLock<BodyDigest> =
        std::sync::LazyLock::new(|| BodyDigest::of(RESPONSE_BODY));
    static TAMPERED_DIGEST: std::sync::LazyLock<BodyDigest> =
        std::sync::LazyLock::new(|| BodyDigest::of(b"tampered"));

    fn fixture_context<'a>(
        receipt: &'a Value,
        identity: &'a EstablishedIdentity,
    ) -> ReceiptContext<'a> {
        ReceiptContext::new(
            receipt,
            identity,
            Some(&REQUEST_DIGEST),
            Some(&RESPONSE_DIGEST),
        )
    }

    #[test]
    fn fixture_receipt_passes_all_receipt_and_upstream_checks() {
        let identity = established_identity(&vector_report()).unwrap();
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let session = vector_session_bytes();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        run_upstream_checks(
            &mut t,
            &receipt,
            UpstreamContext {
                session_bytes: Some(&session),
                no_session_reason: "unused",
                pinned: None,
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );
        for id in [
            "receipt-1",
            "receipt-2",
            "receipt-3",
            "receipt-4",
            "upstream-1",
            "upstream-2",
        ] {
            assert_eq!(status_of(&t, id), Status::Pass, "check {id}");
        }
        assert!(t.verified());
    }

    #[test]
    fn rewrite_note_appears_only_when_forwarded_differs() {
        let identity = established_identity(&vector_report()).unwrap();

        // Equal hashes: the request was untouched, no note.
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        assert!(!t.checks.iter().any(|c| c.def.id == "receipt-note"));

        // Differing hashes are the rewrite: an info line, never a fail.
        let receipt = parse_receipt_document(vector_receipt_envelope_rewritten()).unwrap();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        assert_eq!(status_of(&t, "receipt-note"), Status::Info);
        assert!(t.verified(), "the rewrite note must not block the verdict");
    }

    #[test]
    fn pinned_sessions_enforce_cited_membership() {
        // §9.3(6): with a pinned list (§5.3), the cited id must be in it.
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let session = vector_session_bytes();
        let cited = hex::encode(sha256_raw(
            &jcs_bytes(&serde_json::from_slice::<Value>(&session).unwrap()).unwrap(),
        ));

        // Cited id in the list: upstream-2 passes as usual.
        let mut t = Transcript::default();
        run_upstream_checks(
            &mut t,
            &receipt,
            UpstreamContext {
                session_bytes: Some(&session),
                no_session_reason: "unused",
                pinned: Some(std::slice::from_ref(&cited)),
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );
        assert_eq!(status_of(&t, "upstream-2"), Status::Pass);

        // Cited id absent from the list: upstream-2 fails even though the record
        // itself would verify.
        let mut t = Transcript::default();
        run_upstream_checks(
            &mut t,
            &receipt,
            UpstreamContext {
                session_bytes: Some(&session),
                no_session_reason: "unused",
                pinned: Some(&["ab".repeat(32)]),
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );
        assert_eq!(status_of(&t, "upstream-2"), Status::Fail);
    }

    #[test]
    fn tampered_request_body_fails_r_3() {
        let identity = established_identity(&vector_report()).unwrap();
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let mut cx = fixture_context(&receipt, &identity);
        cx.request_body = Some(&TAMPERED_DIGEST);
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, cx);
        assert_eq!(status_of(&t, "receipt-3"), Status::Fail);
    }

    #[test]
    fn tampered_document_fails_r_1() {
        let identity = established_identity(&vector_report()).unwrap();
        let mut document = vector_receipt_envelope();
        document["receipt_id"] = Value::String("rcpt-tampered".to_string());
        let receipt = parse_receipt_document(document).unwrap();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        assert_eq!(status_of(&t, "receipt-1"), Status::Fail);
    }

    #[test]
    fn reencoded_document_still_verifies_r_1() {
        // §7.2: any encoding of the same document verifies — round-trip the
        // fixture through pretty-printing (which reorders nothing the JCS
        // recomputation cares about) and re-run receipt-1.
        let identity = established_identity(&vector_report()).unwrap();
        let pretty = serde_json::to_string_pretty(&vector_receipt_envelope()).unwrap();
        let document: Value = serde_json::from_str(&pretty).unwrap();
        let receipt = parse_receipt_document(document).unwrap();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        assert_eq!(status_of(&t, "receipt-1"), Status::Pass);
    }

    #[test]
    fn missing_bodies_skip_r_3_and_r_4_without_passing() {
        let identity = established_identity(&vector_report()).unwrap();
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let mut cx = fixture_context(&receipt, &identity);
        cx.request_body = None;
        cx.response_wire = None;
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, cx);
        assert_eq!(status_of(&t, "receipt-3"), Status::Skip);
        assert_eq!(status_of(&t, "receipt-4"), Status::Skip);
        assert!(t.verified()); // skips do not block, and are not passes
        assert_eq!(t.count(Status::Pass), 2);
    }

    #[test]
    fn claims_policy_parses_and_appraises() {
        assert!(RequiredClaim::parse("extra").is_err());
        assert!(RequiredClaim::parse("").is_err());
        assert!(RequiredClaim::parse("tee_attested=nonsense").is_err());

        let record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
        let met = RequiredClaim::parse("tee_attested=hardware_proven").unwrap();
        assert!(unmet_claims(&record, &[met]).is_empty());
        // The fixture's gpu_attested claim is unknown, so requiring it fails.
        let missing = RequiredClaim::parse("gpu_attested").unwrap();
        assert_eq!(
            unmet_claims(&record, &[missing]),
            vec!["gpu_attested".to_string()]
        );
    }

    #[test]
    fn unmet_required_claim_fails_u_2() {
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let session = vector_session_bytes();
        let mut t = Transcript::default();
        run_upstream_checks(
            &mut t,
            &receipt,
            UpstreamContext {
                session_bytes: Some(&session),
                no_session_reason: "unused",
                pinned: None,
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[RequiredClaim::parse("gpu_attested").unwrap()],
            },
        );
        assert_eq!(status_of(&t, "upstream-2"), Status::Fail);
    }

    #[test]
    fn audit_session_record_agrees_with_the_fixture() {
        let bytes = vector_session_bytes();
        let id = private_ai_gateway::aci::digest::sha256_bare_hex(&bytes);
        let record: Value = serde_json::from_slice(&bytes).unwrap();
        let at = record.get("established_at").and_then(Value::as_u64);
        let audit = audit_session_record(&bytes, &id, at).unwrap();
        assert!(audit.integrity_ok());
        let audit = audit_session_record(&bytes, "0".repeat(64).as_str(), at).unwrap();
        assert!(!audit.id_matches);
        assert!(!audit.integrity_ok());
    }

    #[test]
    fn tampered_session_content_fails_u_2() {
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let mut record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
        record["verifier_id"] = Value::String("evil/1".to_string());
        let session = serde_json::to_vec(&record).unwrap();
        let mut t = Transcript::default();
        run_upstream_checks(
            &mut t,
            &receipt,
            UpstreamContext {
                session_bytes: Some(&session),
                no_session_reason: "unused",
                pinned: None,
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );
        assert_eq!(status_of(&t, "upstream-2"), Status::Fail);
    }

    #[test]
    fn reencoded_session_still_passes_u_2() {
        // §8: the id is over the JCS form, so pretty-printing the same
        // content is still the cited session.
        let receipt = parse_receipt_document(vector_receipt_envelope()).unwrap();
        let record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
        let session = serde_json::to_vec_pretty(&record).unwrap();
        let mut t = Transcript::default();
        run_upstream_checks(
            &mut t,
            &receipt,
            UpstreamContext {
                session_bytes: Some(&session),
                no_session_reason: "unused",
                pinned: None,
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );
        assert_eq!(status_of(&t, "upstream-2"), Status::Pass);
    }

    #[test]
    fn session_without_evidence_fails_u_2() {
        // Strip the evidence member and cite the stripped record's own id, so
        // the only failing clause is the §9.3(4) evidence check.
        let mut record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
        record.as_object_mut().unwrap().remove("evidence");
        let bytes = serde_json::to_vec(&record).unwrap();
        let payload = serde_json::json!({
            "served_at": SERVED_AT,
            "event_log": [
                { "type": "upstream.verified", "result": "verified", "required": true,
                  "model_id": "m",
                  "session_id": hex::encode(sha256_raw(&bytes)) },
            ],
        });
        let mut t = Transcript::default();
        run_upstream_checks(
            &mut t,
            &payload,
            UpstreamContext {
                session_bytes: Some(&bytes),
                no_session_reason: "unused",
                pinned: None,
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );
        assert_eq!(status_of(&t, "upstream-2"), Status::Fail);
        let u2 = t.checks.iter().find(|c| c.def.id == "upstream-2").unwrap();
        assert!(u2.detail.contains("no spec 8.2 evidence"), "{}", u2.detail);
    }

    #[test]
    fn foreign_receipt_payload_api_version_is_rejected_at_parse() {
        // Appendix B: reject artifacts whose api_version is not aci/1.
        let mut document = vector_receipt_envelope();
        document["api_version"] = Value::String("aci/2".to_string());
        let Err(err) = parse_receipt_document(document) else {
            panic!("a foreign api_version must be rejected");
        };
        assert!(err.contains("api_version"), "{err}");
    }

    #[test]
    fn foreign_session_api_version_fails_u_2() {
        // Cite the modified record's own id so the only failing clause is the
        // Appendix B api_version gate.
        let mut record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
        record["api_version"] = Value::String("aci/2".to_string());
        let bytes = serde_json::to_vec(&record).unwrap();
        let payload = serde_json::json!({
            "served_at": SERVED_AT,
            "event_log": [
                { "type": "upstream.verified", "result": "verified", "required": true,
                  "model_id": "m",
                  "session_id": hex::encode(sha256_raw(&bytes)) },
            ],
        });
        let mut t = Transcript::default();
        run_upstream_checks(
            &mut t,
            &payload,
            UpstreamContext {
                session_bytes: Some(&bytes),
                no_session_reason: "unused",
                pinned: None,
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );
        assert_eq!(status_of(&t, "upstream-2"), Status::Fail);
        let u2 = t.checks.iter().find(|c| c.def.id == "upstream-2").unwrap();
        assert!(u2.detail.contains("api_version"), "{}", u2.detail);
    }

    #[test]
    fn failed_upstream_event_fails_u_1() {
        let payload = serde_json::json!({
            "served_at": SERVED_AT,
            "event_log": [
                { "type": "request.received", "body_hash": "sha256:aa" },
                { "type": "upstream.verified", "result": "failed", "required": true,
                  "model_id": "m", "reason": "quote verification failed" },
                { "type": "response.returned", "body_hash": "sha256:bb" },
            ],
        });
        let mut t = Transcript::default();
        run_upstream_checks(
            &mut t,
            &payload,
            UpstreamContext {
                session_bytes: None,
                no_session_reason: "no session (failed event)",
                pinned: None,
                requires_verified: true,
                serving: "aggregator",
                required_claims: &[],
            },
        );
        assert_eq!(status_of(&t, "upstream-1"), Status::Fail);
        assert_eq!(status_of(&t, "upstream-2"), Status::Skip);
    }
}
