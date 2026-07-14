//! The check engine shared by every `aci` subcommand: the §10.1 identity
//! checks over an attestation report (L2.1–L2.6), the §10.2 receipt checks
//! (R.1–R.4), and the §10.3 upstream audit (U.1–U.2), per `spec/aci.md`.
//!
//! Subcommands differ only in where the artifacts come from — fetched live
//! (`verify`, `chat`, `serve`) or read from files (`audit`) — which the
//! contexts here express: quote collateral online vs offline, TLS channel
//! observed vs not, bodies supplied vs absent.
//!
//! Artifacts are verified as served bytes (§3): the keyset digest is over the
//! decoded `workload_keyset_b64` bytes, the receipt signature over the decoded
//! `payload_b64` bytes, the session id over the exact fetched body bytes. The
//! binding checks recompute the same §10.1 chain the lib's
//! `validate_aci_report_binding` composes, step by step, so every check gets
//! its own honest status instead of stopping at the first failure.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use private_ai_gateway::aci::digest::{sha256_hex, sha256_raw};
use private_ai_gateway::aci::identity;
use private_ai_gateway::aci::keys::verify_receipt_signature;
use private_ai_gateway::aci::types::{AttestationReport, SourceProvenance, WorkloadKeyset};
use private_ai_gateway::aci::verifier::{
    dcap_report_data, dstack_rtmr3_event, verify_dstack_event_log,
};
use serde_json::Value;

use crate::client::{AciClient, HttpResult};
use crate::transcript::{
    Transcript, L2_1, L2_2, L2_3, L2_4, L2_5, L2_6, R_1, R_2, R_3, R_4, R_NOTE, U_1, U_2,
};

pub enum QuoteCheckMode<'a> {
    /// Fetch DCAP collateral from this PCCS and verify the quote to the
    /// vendor root.
    Online { pccs_url: &'a str },
    /// No collateral available (offline audit): vendor verification is
    /// skipped, never passed. The report-data slot binding is still checked.
    Offline { reason: &'a str },
}

pub enum ChannelObservation<'a> {
    /// The leaf SPKI sha256 observed on the TLS connection that fetched the
    /// report, for the hostname actually used.
    Observed {
        host: &'a str,
        spki_sha256: &'a str,
    },
    NotObserved {
        reason: &'a str,
    },
}

pub struct ReportCheckContext<'a> {
    /// The nonce this verifier supplied on the report fetch (§4.2).
    pub nonce: Option<&'a str>,
    pub now_secs: u64,
    /// Audit `--skip-expiry` (§4.4 archival policy): L2.3 is skipped, never
    /// passed.
    pub expiry_skipped: bool,
    pub quote: QuoteCheckMode<'a>,
    pub channel: ChannelObservation<'a>,
    pub explain: bool,
}

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(value).map_err(|e| e.to_string())
}

/// The workload identity a verified report establishes (§10.1): the keyset
/// parsed from the exact decoded `workload_keyset_b64` bytes, and the digest
/// recomputed over those bytes.
pub struct EstablishedIdentity {
    pub keyset: WorkloadKeyset,
    pub keyset_digest: String,
}

/// Decode + digest + parse the report's keyset, without a transcript. The
/// subcommands use this to pick keys after the transcript reached VERIFIED.
pub fn established_identity(report: &AttestationReport) -> Result<EstablishedIdentity, String> {
    let bytes = BASE64
        .decode(report.attestation.workload_keyset_b64.as_bytes())
        .map_err(|e| format!("workload_keyset_b64 does not decode: {e}"))?;
    let keyset_digest = identity::workload_keyset_digest(&bytes);
    let keyset: WorkloadKeyset = serde_json::from_slice(&bytes)
        .map_err(|e| format!("workload keyset does not parse: {e}"))?;
    Ok(EstablishedIdentity {
        keyset,
        keyset_digest,
    })
}

/// Run the L2.1–L2.6 checks over a parsed report, appending to `transcript`.
///
/// Returns `Err` only for protocol-gate problems (the report is not an
/// `aci/1` report at all); check failures land in the transcript.
pub async fn run_report_checks(
    transcript: &mut Transcript,
    report: &AttestationReport,
    cx: ReportCheckContext<'_>,
) -> Result<(), String> {
    if report.api_version != "aci/1" {
        return Err(format!(
            "unsupported ACI api_version {:?} (expected \"aci/1\")",
            report.api_version
        ));
    }
    transcript.workload_keyset_digest = Some(report.workload_keyset_digest.clone());

    let evidence = &report.attestation.evidence;

    // Decode/parse the quote up front: L2.1 verifies it to the vendor root
    // and checks it binds report_data in the report-data slot.
    let raw_quote = match evidence.get("quote").and_then(Value::as_str) {
        Some(quote_hex) => match decode_hex(quote_hex) {
            Ok(bytes) => Some(bytes),
            Err(e) => {
                transcript.fail(L2_1, format!("evidence quote is not valid hex: {e}"));
                None
            }
        },
        None => {
            transcript.fail(
                L2_1,
                "report evidence carries no quote (hardware evidence is required, fail-closed)",
            );
            None
        }
    };
    let parsed_quote = match &raw_quote {
        Some(bytes) => match dcap_qvl::quote::Quote::parse(bytes) {
            Ok(quote) => Some(quote),
            Err(e) => {
                transcript.fail(L2_1, format!("quote does not parse: {e}"));
                None
            }
        },
        None => None,
    };

    // L2.1 — hardware: quote to vendor root AND report_data bound in the
    // slot (32 bytes zero-padded to 64, §4.2). The slot comparison runs in
    // both modes; vendor verification needs collateral.
    if let (Some(raw), Some(quote)) = (&raw_quote, &parsed_quote) {
        let slot_bound = decode_hex(&report.attestation.report_data_hex)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes.as_slice()).ok())
            .map(identity::report_data_slot)
            .is_some_and(|expected| dcap_report_data(&quote.report) == &expected);
        if !slot_bound {
            transcript.fail(
                L2_1,
                format!(
                    "the quote's report-data slot ({}) does not bind the report's report_data \
                     zero-padded to 64 bytes",
                    hex::encode(dcap_report_data(&quote.report))
                ),
            );
        } else {
            match &cx.quote {
                QuoteCheckMode::Online { pccs_url } => {
                    match dcap_qvl::collateral::get_collateral(pccs_url, raw).await {
                        // Fail closed: verifying against the live service and unable
                        // to fetch collateral means the quote was never checked to
                        // the vendor root — the one thing a forged service cannot
                        // pass. A skip here would let it reach VERIFIED.
                        Err(e) => transcript.fail(
                            L2_1,
                            format!(
                                "quote collateral fetch from {pccs_url} failed: {e}; \
                                 cannot verify the quote to the vendor root without collateral"
                            ),
                        ),
                        Ok(collateral) => {
                            match dcap_qvl::verify::rustcrypto::verify(
                                raw,
                                &collateral,
                                cx.now_secs,
                            ) {
                                Err(e) => transcript
                                    .fail(L2_1, format!("DCAP quote verification failed: {e}")),
                                Ok(verified) => {
                                    let verified_tee = if verified.report.is_sgx() {
                                        "sgx"
                                    } else {
                                        "tdx"
                                    };
                                    if report.attestation.tee_type != verified_tee {
                                        transcript.fail(
                                            L2_1,
                                            format!(
                                                "report claims tee_type {:?} but the quote verified as {verified_tee:?}",
                                                report.attestation.tee_type
                                            ),
                                        );
                                    } else {
                                        transcript.pass(
                                            L2_1,
                                            format!(
                                                "{verified_tee} quote verified (TCB status {}) and binds report_data; \
                                                 collateral from {pccs_url}",
                                                verified.status
                                            ),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
                QuoteCheckMode::Offline { reason } => transcript.skip(
                    L2_1,
                    format!(
                        "the quote binds report_data, but {reason}; run `aci verify` against \
                         the live service to check it to the vendor root"
                    ),
                    *reason,
                ),
            }
        }
        if cx.explain {
            transcript.explain(format!(
                "report_data (32 bytes) = {}\nquote report_data slot (64 bytes) = {}",
                report.attestation.report_data_hex,
                hex::encode(dcap_report_data(&quote.report))
            ));
        }
    }

    // L2.2 — the binding chain, recomputed from served bytes (§10.1(2)):
    // base64-decode the keyset; sha256(bytes) == workload_keyset_digest;
    // build the §4.2 statement for our nonce; sha256(statement) == report_data.
    let mut established: Option<EstablishedIdentity> = None;
    {
        let nonce_desc = match cx.nonce {
            Some(nonce) => format!("nonce {nonce:?}"),
            None => "null nonce".to_string(),
        };
        let keyset_bytes = BASE64
            .decode(report.attestation.workload_keyset_b64.as_bytes())
            .map_err(|e| format!("workload_keyset_b64 does not decode: {e}"));
        match keyset_bytes {
            Err(e) => transcript.fail(L2_2, e),
            Ok(bytes) => {
                let computed_digest = identity::workload_keyset_digest(&bytes);
                let keyset: Result<WorkloadKeyset, _> = serde_json::from_slice(&bytes);
                let statement = identity::attestation_statement(&computed_digest, cx.nonce);
                let explain_text = cx.explain.then(|| {
                    let statement_text = statement
                        .as_ref()
                        .map(|s| String::from_utf8_lossy(s).into_owned())
                        .unwrap_or_else(|e| format!("<invalid nonce: {e}>"));
                    format!(
                        "keyset bytes ({} bytes): {}\ncomputed digest: {computed_digest}\nstatement: {statement_text}\ncomputed report_data: {}\nexpected report_data: {}",
                        bytes.len(),
                        String::from_utf8_lossy(&bytes),
                        statement
                            .as_ref()
                            .map(|s| hex::encode(identity::report_data(s)))
                            .unwrap_or_default(),
                        report.attestation.report_data_hex,
                    )
                });
                if computed_digest != report.workload_keyset_digest {
                    transcript.fail(
                        L2_2,
                        format!(
                            "sha256 over the decoded keyset bytes is {computed_digest}, report claims {}",
                            report.workload_keyset_digest
                        ),
                    );
                } else if let Err(e) = &keyset {
                    transcript.fail(L2_2, format!("workload keyset does not parse: {e}"));
                } else {
                    match statement {
                        Err(e) => transcript.fail(L2_2, format!("invalid nonce: {e}")),
                        Ok(statement) => {
                            let expected = identity::report_data(&statement);
                            let reported = decode_hex(&report.attestation.report_data_hex)
                                .ok()
                                .and_then(|b| <[u8; 32]>::try_from(b.as_slice()).ok());
                            match reported {
                                Some(reported) if reported == expected => {
                                    transcript.pass(
                                        L2_2,
                                        format!(
                                            "keyset digest {computed_digest}; statement digest for {nonce_desc} matches report_data"
                                        ),
                                    );
                                    established = keyset.ok().map(|keyset| EstablishedIdentity {
                                        keyset,
                                        keyset_digest: computed_digest,
                                    });
                                }
                                Some(reported) => transcript.fail(
                                    L2_2,
                                    format!(
                                        "statement digest for {nonce_desc} is {}, report carries {}",
                                        hex::encode(expected),
                                        hex::encode(reported)
                                    ),
                                ),
                                None => transcript.fail(
                                    L2_2,
                                    format!(
                                        "report_data {:?} is not 32 bytes of hex",
                                        report.attestation.report_data_hex
                                    ),
                                ),
                            }
                        }
                    }
                }
                if let Some(text) = explain_text {
                    transcript.explain(text);
                }
            }
        }
    }
    let keyset = established.as_ref().map(|id| &id.keyset);

    // L2.3 — keyset expiry (§10.1(3)): now < not_after in the decoded keyset.
    if cx.expiry_skipped {
        transcript.skip(
            L2_3,
            "keyset expiry not evaluated (--skip-expiry; archival policy, §4.4)",
            "expiry skipped by flag",
        );
    } else {
        match keyset {
            None => transcript.fail(L2_3, "no decoded keyset to read not_after from (see L2.2)"),
            Some(keyset) if cx.now_secs < keyset.not_after => transcript.pass(
                L2_3,
                format!("now {} < not_after {}", cx.now_secs, keyset.not_after),
            ),
            Some(keyset) => transcript.fail(
                L2_3,
                format!(
                    "keyset EXPIRED: now {} >= not_after {}",
                    cx.now_secs, keyset.not_after
                ),
            ),
        }
    }

    // L2.4 — source provenance (§10.1(4)): verify the booted compose is the
    // one measured into the quote, when the service publishes it.
    check_source_provenance(
        transcript,
        evidence,
        parsed_quote.as_ref().map(|quote| &quote.report),
        &report.attestation.source_provenance,
        cx.explain,
    );

    // L2.5 — key custody and subject per profile. The in-tree dstack KMS
    // chain validation is not exported to this CLI yet; the subject is
    // surfaced for the caller's own policy.
    transcript.skip(
        L2_5,
        format!(
            "custody profile not implemented in this CLI yet (see src/aci/verifier/dstack.rs); \
             subject: {} (no profile constraints applied)",
            keyset.and_then(|k| k.subject.as_deref()).unwrap_or("null")
        ),
        "custody profile not implemented",
    );

    // L2.6 — the channel actually used (§10.1(6)). Domain-scoped keyset
    // entries (§4.1) are matched against the hostname the connection targeted.
    match &cx.channel {
        ChannelObservation::NotObserved { reason } => {
            transcript.skip(L2_6, *reason, "no TLS channel observed")
        }
        ChannelObservation::Observed { host, spki_sha256 } => {
            let Some(keyset) = keyset else {
                transcript.fail(
                    L2_6,
                    "no decoded keyset to match the channel against (see L2.2)",
                );
                return Ok(());
            };
            let host = host.to_ascii_lowercase();
            let observed = spki_sha256.to_ascii_lowercase();
            let domain_scoped = keyset.tls_public_keys.iter().any(|k| k.domain.is_some());
            let candidates: Vec<&str> = keyset
                .tls_public_keys
                .iter()
                .filter(|k| {
                    !domain_scoped
                        || k.domain.as_deref().is_some_and(|d| {
                            d.trim().trim_end_matches('.').eq_ignore_ascii_case(&host)
                        })
                })
                .map(|k| k.spki_sha256_hex.as_str())
                .collect();
            if keyset.tls_public_keys.is_empty() {
                transcript.fail(L2_6, "keyset publishes no tls_public_keys");
            } else if candidates.is_empty() {
                transcript.fail(
                    L2_6,
                    format!("no attested TLS key is scoped to hostname {host}"),
                );
            } else if candidates.iter().any(|c| c.eq_ignore_ascii_case(&observed)) {
                transcript.pass(
                    L2_6,
                    format!("observed SPKI {observed} for {host} is in the attested keyset"),
                );
            } else {
                transcript.fail(
                    L2_6,
                    format!("observed SPKI {observed} for {host} is NOT in the attested keyset"),
                );
            }
            if cx.explain {
                transcript.explain(format!(
                    "observed leaf SPKI sha256 for {host}: {observed}\nattested candidates for this hostname: {}",
                    if candidates.is_empty() { "(none)".to_string() } else { candidates.join(", ") }
                ));
            }
        }
    }

    Ok(())
}

/// L2.4 — source provenance (§10.1(4)): when the service publishes the booted
/// `app_compose`, verify it is the compose measured into the quote's RTMR3
/// (`sha256(app_compose)` == the RTMR3 `compose-hash` event). Repo/commit stay
/// supplementary; older services without `app_compose` skip rather than pass.
fn check_source_provenance(
    transcript: &mut Transcript,
    evidence: &Value,
    quote_report: Option<&dcap_qvl::quote::Report>,
    provenance: &SourceProvenance,
    explain: bool,
) {
    let prov = match (
        provenance.repo_url.as_deref(),
        provenance.repo_commit.as_deref(),
        provenance.image_digest.as_deref(),
    ) {
        (Some(url), Some(commit), _) => format!("repo={url} commit={commit}"),
        (_, _, Some(digest)) => format!("image_digest={digest}"),
        _ => "no source provenance published".to_string(),
    };
    let (Some(app_compose), Some(report)) = (
        evidence.get("app_compose").and_then(Value::as_str),
        quote_report,
    ) else {
        transcript.skip(
            L2_4,
            format!("service does not publish app_compose; provenance is presence-only: {prov}"),
            "no app_compose",
        );
        return;
    };
    let events = match verify_dstack_event_log(evidence, report) {
        Ok(events) => events,
        Err(e) => return transcript.fail(L2_4, format!("dstack event log did not verify: {e}")),
    };
    let recomputed = hex::encode(sha256_raw(app_compose.as_bytes()));
    let measured = dstack_rtmr3_event(&events, "compose-hash").map(|e| e.event_payload.as_str());
    match measured {
        None => transcript.fail(L2_4, "verified event log carries no compose-hash event"),
        Some(h) if h.eq_ignore_ascii_case(&recomputed) => transcript.pass(
            L2_4,
            format!("booted compose measured into RTMR3: compose-hash={recomputed}; {prov} (published, not independently rebuilt)"),
        ),
        Some(h) => transcript.fail(
            L2_4,
            format!("sha256(app_compose)={recomputed} != measured compose-hash={h}"),
        ),
    }
    if explain {
        transcript.explain(format!(
            "sha256(app_compose) = {recomputed}\nmeasured compose-hash = {}\nRTMR3 replay matched the quote",
            measured.unwrap_or("(none)")
        ));
    }
}

/// A fetched receipt: the §8.2 signed-bytes envelope, the exact decoded
/// payload bytes the signature covers, and the payload parsed for reading.
pub struct FetchedReceipt {
    pub envelope: Value,
    pub payload_bytes: Vec<u8>,
    pub payload: Value,
}

/// Parse the §8.2 envelope; the payload is decoded from `payload_b64` and
/// kept as exact bytes (§3: the bytes are the artifact).
pub fn parse_receipt_envelope(envelope: Value) -> Result<FetchedReceipt, String> {
    let payload_b64 = envelope
        .get("payload_b64")
        .and_then(Value::as_str)
        .ok_or("receipt envelope has no payload_b64")?;
    let payload_bytes = BASE64
        .decode(payload_b64.as_bytes())
        .map_err(|e| format!("receipt payload_b64 does not decode: {e}"))?;
    let payload: Value = serde_json::from_slice(&payload_bytes)
        .map_err(|e| format!("receipt payload is not JSON: {e}"))?;
    // Appendix A: artifacts with a foreign api_version are rejected, same as
    // the report gate in run_report_checks.
    if field_str(&payload, "api_version") != Some("aci/1") {
        return Err(format!(
            "unsupported receipt payload api_version {:?} (expected \"aci/1\")",
            payload.get("api_version").unwrap_or(&Value::Null)
        ));
    }
    Ok(FetchedReceipt {
        envelope,
        payload_bytes,
        payload,
    })
}

pub struct ReceiptContext<'a> {
    pub receipt: &'a FetchedReceipt,
    /// The established keyset (§10.1) whose `receipt_signing_keys` resolve
    /// the envelope `key_id`, plus its recomputed digest.
    pub keyset: &'a WorkloadKeyset,
    pub workload_keyset_digest: &'a str,
    /// The exact request body bytes the client sent, when available.
    pub request_body: Option<&'a [u8]>,
    /// The exact response body bytes as read off the wire, when available.
    pub response_wire: Option<&'a [u8]>,
    pub explain: bool,
}

impl<'a> ReceiptContext<'a> {
    /// Receipt context for an established identity and the exact body bytes. The
    /// subcommands render receipts without `--explain`, so it is off here.
    pub fn new(
        receipt: &'a FetchedReceipt,
        identity: &'a EstablishedIdentity,
        request_body: Option<&'a [u8]>,
        response_wire: Option<&'a [u8]>,
    ) -> Self {
        Self {
            receipt,
            keyset: &identity.keyset,
            workload_keyset_digest: &identity.keyset_digest,
            request_body,
            response_wire,
            explain: false,
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
/// to — the handle for the §10.3 deep audit. Filtering on the verified result
/// keeps the fetch and U.2 on the same session U.1 blessed, even when §8.5
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
/// artifact source for the U.2 deep audit. Returns the raw 2xx response (the
/// exact served bytes hash to the session id, §9) and otherwise the reason
/// U.2 cites for skipping.
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

/// Run R.1–R.4 (§10.2) over a receipt envelope against an established identity.
pub fn run_receipt_checks(transcript: &mut Transcript, cx: ReceiptContext<'_>) {
    // R.1 — envelope signature over the decoded payload bytes, under the
    // attested keyset entry named by key_id; the keyset entry decides the
    // algorithm and the envelope algo must match it (§8.2).
    check_signature(transcript, &cx);

    // R.2 — the payload binds back to the established keyset digest.
    let payload_digest = field_str(&cx.receipt.payload, "workload_keyset_digest");
    if payload_digest == Some(cx.workload_keyset_digest) {
        transcript.pass(
            R_2,
            "payload workload_keyset_digest matches the established digest",
        );
    } else {
        transcript.fail(
            R_2,
            format!(
                "payload carries workload_keyset_digest {payload_digest:?}, established {}",
                cx.workload_keyset_digest
            ),
        );
    }

    // R.3 — request.received.body_hash covers the bytes the client sent (for
    // E2EE requests, the original bytes the client sealed, §8.4).
    match cx.request_body {
        None => transcript.skip(
            R_3,
            "request body bytes not supplied",
            "request bytes not supplied",
        ),
        Some(body) => {
            let computed = sha256_hex(body);
            match event_by_type(&cx.receipt.payload, "request.received")
                .and_then(|event| field_str(event, "body_hash"))
            {
                None => transcript.fail(R_3, "receipt has no request.received body_hash"),
                Some(recorded) if recorded == computed => {
                    transcript.pass(R_3, format!("{computed} over {} bytes", body.len()));
                }
                Some(recorded) => transcript.fail(
                    R_3,
                    format!("computed {computed}, receipt records {recorded}"),
                ),
            }
        }
    }

    // R.4 — response.returned covers the exact bytes read off the wire (raw
    // SSE bytes for a stream, the sealed envelope bytes for E2EE, §8.4).
    match cx.response_wire {
        None => transcript.skip(
            R_4,
            "response wire bytes not supplied",
            "response bytes not supplied",
        ),
        Some(bytes) => {
            let computed = sha256_hex(bytes);
            match event_by_type(&cx.receipt.payload, "response.returned")
                .and_then(|event| field_str(event, "body_hash"))
            {
                None => transcript.fail(R_4, "receipt has no response.returned body_hash"),
                Some(recorded) if recorded == computed => {
                    transcript.pass(R_4, format!("{computed} over {} bytes", bytes.len()));
                }
                Some(recorded) => transcript.fail(
                    R_4,
                    format!("computed {computed}, receipt records {recorded}"),
                ),
            }
        }
    }

    // §10.2 rewrite note: differing request.forwarded/request.received hashes
    // are the service-side rewrite. ACI records it, nothing more — whether a
    // rewrite is acceptable is local policy, so this is an info line.
    let received = event_by_type(&cx.receipt.payload, "request.received")
        .and_then(|event| field_str(event, "body_hash"));
    let forwarded = event_by_type(&cx.receipt.payload, "request.forwarded")
        .and_then(|event| field_str(event, "body_hash"));
    if let (Some(received), Some(forwarded)) = (received, forwarded) {
        if received != forwarded {
            transcript.info(
                R_NOTE,
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
    let Some(key_id) = field_str(&cx.receipt.envelope, "key_id") else {
        transcript.fail(R_1, "receipt envelope has no key_id");
        return;
    };
    let Some(receipt_key) = cx
        .keyset
        .receipt_signing_keys
        .iter()
        .find(|key| key.key_id == key_id)
    else {
        transcript.fail(
            R_1,
            format!("envelope key_id {key_id:?} is not in the attested keyset"),
        );
        return;
    };
    // The attested keyset entry decides the algorithm, never the artifact
    // (§3); the envelope's algo must agree with it.
    if field_str(&cx.receipt.envelope, "algo") != Some(receipt_key.algo.as_str()) {
        transcript.fail(
            R_1,
            format!(
                "envelope algo {:?} does not match the attested keyset entry algo {:?}",
                field_str(&cx.receipt.envelope, "algo"),
                receipt_key.algo
            ),
        );
        return;
    }
    let Some(signature_hex) = field_str(&cx.receipt.envelope, "signature") else {
        transcript.fail(R_1, "receipt envelope has no signature");
        return;
    };
    let Ok(signature) = hex::decode(signature_hex) else {
        transcript.fail(R_1, "receipt envelope signature is not hex");
        return;
    };
    if verify_receipt_signature(receipt_key, &cx.receipt.payload_bytes, &signature) {
        transcript.pass(
            R_1,
            format!(
                "{} signature by attested key {key_id:?} verifies over the decoded payload bytes",
                receipt_key.algo
            ),
        );
    } else {
        transcript.fail(R_1, format!("signature by key {key_id:?} does not verify"));
    }
    if cx.explain {
        transcript.explain(format!(
            "signed bytes (decoded payload_b64): {}\nsha256(signed bytes) = {}\nsignature ({}): {signature_hex}",
            String::from_utf8_lossy(&cx.receipt.payload_bytes),
            sha256_hex(&cx.receipt.payload_bytes),
            receipt_key.algo
        ));
    }
}

/// Run U.1 (and U.2 when the session's exact served bytes are supplied) per
/// §10.3 over a receipt payload.
pub fn run_upstream_checks(
    transcript: &mut Transcript,
    payload: &Value,
    session_bytes: Option<&[u8]>,
    no_session_reason: &str,
    explain: bool,
) {
    // U.1 — the serving upstream was verified before the prompt was forwarded
    // and the event cites the session holding the verification detail.
    let upstream_events: Vec<&Value> = events(payload)
        .filter(|event| field_str(event, "type") == Some("upstream.verified"))
        .collect();
    if upstream_events.is_empty() {
        transcript.fail(U_1, "receipt carries no upstream.verified event");
    } else {
        // The serving upstream is the verified event; §8.5 allows prior failed
        // attempts alongside it, so they are informational, not a U.1 failure.
        let others = upstream_events.len() - 1;
        let note = if others > 0 {
            format!(" ({others} prior attempt(s))")
        } else {
            String::new()
        };
        match upstream_events
            .iter()
            .find(|event| field_str(event, "result") == Some("verified"))
        {
            None => transcript.fail(
                U_1,
                "no upstream.verified event reports a verified upstream",
            ),
            Some(event) if event.get("required").and_then(Value::as_bool) != Some(true) => {
                // §10.3(1): a client that requires verified upstreams rejects
                // receipts where `required` is false.
                transcript.fail(U_1, "verified upstream but required is not true")
            }
            Some(event) => match field_str(event, "session_id") {
                None => transcript.fail(U_1, "verified upstream but cites no session_id"),
                Some(session) => transcript.pass(
                    U_1,
                    format!(
                        "model={} session={session}{note}",
                        field_str(event, "model_id").unwrap_or("?")
                    ),
                ),
            },
        }
    }

    // U.2 — deep audit of the attested session record: the exact served bytes
    // hash to the cited id (§9), the receipt's served_at falls in the
    // session's validity window, and the evidence data hashes to its digest.
    let Some(bytes) = session_bytes else {
        transcript.skip(U_2, no_session_reason, "no session record");
        return;
    };
    let Some(cited) = session_id_from_receipt(payload) else {
        transcript.fail(U_2, "receipt cites no session_id to audit against");
        return;
    };
    let recomputed = format!("sha256:{}", hex::encode(sha256_raw(bytes)));
    let record: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(e) => {
            transcript.fail(U_2, format!("session record is not JSON: {e}"));
            return;
        }
    };
    // Appendix A: a session document with a foreign api_version is rejected.
    let version_ok = field_str(&record, "api_version") == Some("aci/1");
    let id_matches = recomputed == cited;
    let served_at = payload.get("served_at").and_then(Value::as_u64);
    let window = (
        record.get("established_at").and_then(Value::as_u64),
        record.get("expires_at").and_then(Value::as_u64),
    );
    let in_window = match (served_at, window) {
        (Some(at), (Some(from), Some(until))) => from <= at && at <= until,
        _ => false,
    };
    let evidence = evidence_check(record.get("evidence"));
    let evidence_ok = evidence.is_ok();
    let clause = |ok: bool, yes: &str, no: &str| if ok { yes.to_string() } else { no.to_string() };
    let mut detail = format!(
        "session {cited}: {}; {}; {}; claims: {}",
        clause(
            id_matches,
            "served bytes hash to the cited id",
            &format!("served bytes hash to {recomputed}, NOT the cited id")
        ),
        clause(
            in_window,
            "receipt served_at inside the validity window",
            "receipt served_at OUTSIDE the validity window"
        ),
        match &evidence {
            Ok(()) => "evidence data hashes to its digest".to_string(),
            Err(reason) => reason.clone(),
        },
        claims_summary(record.get("claims")),
    );
    if !version_ok {
        detail = format!(
            "record api_version {:?} is not \"aci/1\"; {detail}",
            record.get("api_version").unwrap_or(&Value::Null)
        );
    }
    if version_ok && id_matches && in_window && evidence_ok {
        transcript.pass(U_2, detail);
    } else {
        transcript.fail(U_2, detail);
    }
    if explain {
        transcript.explain(format!(
            "session bytes ({} bytes)\ncomputed: sha256 over served bytes = {recomputed}\nexpected: receipt-cited session_id = {cited}",
            bytes.len(),
        ));
    }
}

/// Typed-claims one-liner for the U.2 detail (shallow audit surface, §10.3(5)).
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

/// §10.3(4): `evidence.data` decodes and hashes to `evidence.digest` (§9.2:
/// a record whose data does not match its digest MUST be rejected). Missing
/// or malformed evidence rejects too — the deep audit never assumes.
fn evidence_check(evidence: Option<&Value>) -> Result<(), String> {
    let (Some(digest), Some(data_uri)) = (
        evidence.and_then(|e| field_str(e, "digest")),
        evidence.and_then(|e| field_str(e, "data")),
    ) else {
        return Err("record carries no §9.2 evidence digest+data".to_string());
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
            quote: QuoteCheckMode::Offline {
                reason: "quote collateral offline",
            },
            channel: ChannelObservation::NotObserved {
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

        assert_eq!(status_of(&t, "L2.2"), Status::Pass);
        assert_eq!(status_of(&t, "L2.3"), Status::Pass);
        // The fixture report carries no hardware quote and no app_compose, so
        // L2.1 fails closed and L2.4 skips honestly; nothing here may pass.
        assert_eq!(status_of(&t, "L2.1"), Status::Fail);
        assert_eq!(status_of(&t, "L2.4"), Status::Skip);
        assert_eq!(status_of(&t, "L2.5"), Status::Skip);
        assert_eq!(status_of(&t, "L2.6"), Status::Skip);
        assert!(!t.verified());
        assert_eq!(
            t.workload_keyset_digest.as_deref(),
            Some(report.workload_keyset_digest.as_str())
        );

        // The lib validator (which checks exactly the binding subset) agrees.
        validate_aci_report_binding(&report, Some(TEST_NONCE), now, None).unwrap();
    }

    fn td10_report(rt_mr3: [u8; 48]) -> dcap_qvl::quote::Report {
        dcap_qvl::quote::Report::TD10(dcap_qvl::quote::TDReport10 {
            tee_tcb_svn: [0; 16],
            mr_seam: [0; 48],
            mr_signer_seam: [0; 48],
            seam_attributes: [0; 8],
            td_attributes: [0; 8],
            xfam: [0; 8],
            mr_td: [0; 48],
            mr_config_id: [0; 48],
            mr_owner: [0; 48],
            mr_owner_config: [0; 48],
            rt_mr0: [0; 48],
            rt_mr1: [0; 48],
            rt_mr2: [0; 48],
            rt_mr3,
            report_data: [0; 64],
        })
    }

    #[test]
    fn compose_measurement_l2_4_passes_and_fails_on_mismatch() {
        use sha2::{Digest, Sha384};

        // Two RTMR3 boot events: a compose-hash carrying sha256(app_compose),
        // then system-ready. RTMR3 is the SHA-384 chain over their 48-byte
        // digests from a 48-byte-zero start (dstack replay); build the quote to
        // match, so verify_dstack_event_log accepts and L2.4 reads the compose.
        let app_compose = "services:\n  gateway:\n    image: demo\n";
        let compose_hash = hex::encode(sha256_raw(app_compose.as_bytes()));
        let digests = [[0x11u8; 48], [0x22u8; 48]];
        let mut mr = vec![0u8; 48];
        for d in digests {
            mr.extend_from_slice(&d);
            mr = Sha384::digest(&mr).to_vec();
        }
        let report = td10_report(mr.as_slice().try_into().unwrap());
        let mut evidence = serde_json::json!({
            "event_log": serde_json::to_string(&serde_json::json!([
                { "imr": 3, "digest": hex::encode(digests[0]),
                  "event": "compose-hash", "event_payload": compose_hash },
                { "imr": 3, "digest": hex::encode(digests[1]),
                  "event": "system-ready", "event_payload": "" },
            ]))
            .unwrap(),
            "app_compose": app_compose,
        });
        let provenance = SourceProvenance {
            repo_url: Some("https://example.com/repo".to_string()),
            repo_commit: Some("abc123".to_string()),
            image_digest: None,
            image_provenance: None,
        };

        let mut t = Transcript::default();
        check_source_provenance(&mut t, &evidence, Some(&report), &provenance, false);
        assert_eq!(status_of(&t, "L2.4"), Status::Pass);

        // A different app_compose no longer matches the measured compose-hash.
        evidence["app_compose"] = Value::String("tampered".to_string());
        let mut t = Transcript::default();
        check_source_provenance(&mut t, &evidence, Some(&report), &provenance, false);
        assert_eq!(status_of(&t, "L2.4"), Status::Fail);
    }

    #[tokio::test]
    async fn tampered_keyset_digest_fails_l2_2() {
        let mut report = vector_report();
        report.workload_keyset_digest =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000".to_string();
        let mut t = Transcript::default();
        run_report_checks(&mut t, &report, offline_cx(Some(TEST_NONCE), SERVED_AT))
            .await
            .unwrap();
        assert_eq!(status_of(&t, "L2.2"), Status::Fail);
        assert!(validate_aci_report_binding(&report, Some(TEST_NONCE), SERVED_AT, None).is_err());
    }

    #[tokio::test]
    async fn wrong_nonce_fails_l2_2() {
        let report = vector_report();
        let mut t = Transcript::default();
        run_report_checks(
            &mut t,
            &report,
            offline_cx(Some("some-other-nonce"), SERVED_AT),
        )
        .await
        .unwrap();
        assert_eq!(status_of(&t, "L2.2"), Status::Fail);
        assert!(
            validate_aci_report_binding(&report, Some("some-other-nonce"), SERVED_AT, None)
                .is_err()
        );
    }

    #[tokio::test]
    async fn expired_keyset_fails_l2_3_and_skip_expiry_skips_it() {
        let report = vector_report();
        let after_expiry = KEYSET_NOT_AFTER + 1;
        let mut t = Transcript::default();
        run_report_checks(&mut t, &report, offline_cx(Some(TEST_NONCE), after_expiry))
            .await
            .unwrap();
        assert_eq!(status_of(&t, "L2.3"), Status::Fail);

        let mut t = Transcript::default();
        let mut cx = offline_cx(Some(TEST_NONCE), after_expiry);
        cx.expiry_skipped = true;
        run_report_checks(&mut t, &report, cx).await.unwrap();
        assert_eq!(status_of(&t, "L2.3"), Status::Skip);
    }

    #[tokio::test]
    async fn channel_binding_matches_domain_scoped_entry() {
        let report = vector_report();
        let identity = established_identity(&report).unwrap();
        let spki = identity.keyset.tls_public_keys[0].spki_sha256_hex.clone();
        let domain = identity.keyset.tls_public_keys[0].domain.clone().unwrap();

        let mut t = Transcript::default();
        let mut cx = offline_cx(Some(TEST_NONCE), SERVED_AT);
        cx.channel = ChannelObservation::Observed {
            host: &domain,
            spki_sha256: &spki,
        };
        run_report_checks(&mut t, &report, cx).await.unwrap();
        assert_eq!(status_of(&t, "L2.6"), Status::Pass);

        // Same SPKI presented for a hostname the keyset does not scope it to.
        let mut t = Transcript::default();
        let mut cx = offline_cx(Some(TEST_NONCE), SERVED_AT);
        cx.channel = ChannelObservation::Observed {
            host: "other.example.com",
            spki_sha256: &spki,
        };
        run_report_checks(&mut t, &report, cx).await.unwrap();
        assert_eq!(status_of(&t, "L2.6"), Status::Fail);
    }

    fn fixture_context<'a>(
        receipt: &'a FetchedReceipt,
        identity: &'a EstablishedIdentity,
    ) -> ReceiptContext<'a> {
        ReceiptContext::new(receipt, identity, Some(REQUEST_BODY), Some(RESPONSE_BODY))
    }

    #[test]
    fn fixture_receipt_passes_all_receipt_and_upstream_checks() {
        let identity = established_identity(&vector_report()).unwrap();
        let receipt = parse_receipt_envelope(vector_receipt_envelope()).unwrap();
        let session = vector_session_bytes();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        run_upstream_checks(&mut t, &receipt.payload, Some(&session), "unused", false);
        for id in ["R.1", "R.2", "R.3", "R.4", "U.1", "U.2"] {
            assert_eq!(status_of(&t, id), Status::Pass, "check {id}");
        }
        assert!(t.verified());
    }

    #[test]
    fn rewrite_note_appears_only_when_forwarded_differs() {
        let identity = established_identity(&vector_report()).unwrap();

        // Equal hashes: the request was untouched, no note.
        let receipt = parse_receipt_envelope(vector_receipt_envelope()).unwrap();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        assert!(!t.checks.iter().any(|c| c.def.id == "R.note"));

        // Differing hashes are the rewrite: an info line, never a fail.
        let receipt = parse_receipt_envelope(vector_receipt_envelope_rewritten()).unwrap();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        assert_eq!(status_of(&t, "R.note"), Status::Info);
        assert!(t.verified(), "the rewrite note must not block the verdict");
    }

    #[test]
    fn tampered_request_body_fails_r_3() {
        let identity = established_identity(&vector_report()).unwrap();
        let receipt = parse_receipt_envelope(vector_receipt_envelope()).unwrap();
        let mut cx = fixture_context(&receipt, &identity);
        cx.request_body = Some(b"tampered");
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, cx);
        assert_eq!(status_of(&t, "R.3"), Status::Fail);
    }

    #[test]
    fn tampered_payload_fails_r_1() {
        let identity = established_identity(&vector_report()).unwrap();
        let mut envelope = vector_receipt_envelope();
        // Flip one payload byte (inside a string, so it stays JSON) through
        // the base64 transport encoding.
        let mut payload = BASE64
            .decode(envelope["payload_b64"].as_str().unwrap())
            .unwrap();
        let at = payload
            .windows(4)
            .position(|w| w == b"rcpt")
            .expect("receipt id in payload");
        payload[at] ^= 1;
        envelope["payload_b64"] = Value::String(BASE64.encode(&payload));
        let receipt = parse_receipt_envelope(envelope).unwrap();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        assert_eq!(status_of(&t, "R.1"), Status::Fail);
    }

    #[test]
    fn envelope_algo_must_match_the_keyset_entry() {
        let identity = established_identity(&vector_report()).unwrap();
        let mut envelope = vector_receipt_envelope();
        envelope["algo"] = Value::String("secp256k1".to_string());
        let receipt = parse_receipt_envelope(envelope).unwrap();
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, fixture_context(&receipt, &identity));
        assert_eq!(status_of(&t, "R.1"), Status::Fail);
    }

    #[test]
    fn missing_bodies_skip_r_3_and_r_4_without_passing() {
        let identity = established_identity(&vector_report()).unwrap();
        let receipt = parse_receipt_envelope(vector_receipt_envelope()).unwrap();
        let mut cx = fixture_context(&receipt, &identity);
        cx.request_body = None;
        cx.response_wire = None;
        let mut t = Transcript::default();
        run_receipt_checks(&mut t, cx);
        assert_eq!(status_of(&t, "R.3"), Status::Skip);
        assert_eq!(status_of(&t, "R.4"), Status::Skip);
        assert!(t.verified()); // skips do not block, and are not passes
        assert_eq!(t.count(Status::Pass), 2);
    }

    #[test]
    fn tampered_session_bytes_fail_u_2() {
        let receipt = parse_receipt_envelope(vector_receipt_envelope()).unwrap();
        let mut session = vector_session_bytes();
        // One byte of whitespace changes the served bytes, hence the id (§9).
        session.push(b' ');
        let mut t = Transcript::default();
        run_upstream_checks(&mut t, &receipt.payload, Some(&session), "unused", false);
        assert_eq!(status_of(&t, "U.2"), Status::Fail);
    }

    #[test]
    fn session_without_evidence_fails_u_2() {
        // Strip the evidence member and cite the stripped record's own id, so
        // the only failing clause is the §10.3(4) evidence check.
        let mut record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
        record.as_object_mut().unwrap().remove("evidence");
        let bytes = serde_json::to_vec(&record).unwrap();
        let payload = serde_json::json!({
            "served_at": SERVED_AT,
            "event_log": [
                { "type": "upstream.verified", "result": "verified", "required": true,
                  "model_id": "m",
                  "session_id": format!("sha256:{}", hex::encode(sha256_raw(&bytes))) },
            ],
        });
        let mut t = Transcript::default();
        run_upstream_checks(&mut t, &payload, Some(&bytes), "unused", false);
        assert_eq!(status_of(&t, "U.2"), Status::Fail);
        let u2 = t.checks.iter().find(|c| c.def.id == "U.2").unwrap();
        assert!(u2.detail.contains("no §9.2 evidence"), "{}", u2.detail);
    }

    #[test]
    fn foreign_receipt_payload_api_version_is_rejected_at_parse() {
        // Appendix A: reject artifacts whose api_version is not aci/1.
        let mut envelope = vector_receipt_envelope();
        let mut payload: Value = serde_json::from_slice(
            &BASE64
                .decode(envelope["payload_b64"].as_str().unwrap())
                .unwrap(),
        )
        .unwrap();
        payload["api_version"] = Value::String("aci/2".to_string());
        envelope["payload_b64"] =
            Value::String(BASE64.encode(serde_json::to_vec(&payload).unwrap()));
        let Err(err) = parse_receipt_envelope(envelope) else {
            panic!("a foreign api_version must be rejected");
        };
        assert!(err.contains("api_version"), "{err}");
    }

    #[test]
    fn foreign_session_api_version_fails_u_2() {
        // Cite the modified record's own id so the only failing clause is the
        // Appendix A api_version gate.
        let mut record: Value = serde_json::from_slice(&vector_session_bytes()).unwrap();
        record["api_version"] = Value::String("aci/2".to_string());
        let bytes = serde_json::to_vec(&record).unwrap();
        let payload = serde_json::json!({
            "served_at": SERVED_AT,
            "event_log": [
                { "type": "upstream.verified", "result": "verified", "required": true,
                  "model_id": "m",
                  "session_id": format!("sha256:{}", hex::encode(sha256_raw(&bytes))) },
            ],
        });
        let mut t = Transcript::default();
        run_upstream_checks(&mut t, &payload, Some(&bytes), "unused", false);
        assert_eq!(status_of(&t, "U.2"), Status::Fail);
        let u2 = t.checks.iter().find(|c| c.def.id == "U.2").unwrap();
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
        run_upstream_checks(&mut t, &payload, None, "no session (failed event)", false);
        assert_eq!(status_of(&t, "U.1"), Status::Fail);
        assert_eq!(status_of(&t, "U.2"), Status::Skip);
    }
}
