//! `aci sessions`: audit the attested sessions a service currently serves from.
//!
//! Verifies the service identity (spec 9.1, fail closed), lists the current
//! sessions, fetches each full record by id, and runs the spec 9.2 audit
//! plus the `--require-claim` policy on it. The accepted ids are what a
//! client pins (spec 5.3), so this is the prevention-side counterpart of
//! the receipt audit.

use serde_json::{json, Value};

use crate::args::SessionsArgs;
use crate::checks::{now_secs, unmet_claims, RequiredClaim, SessionAudit};
use crate::client::AciClient;
use crate::verify::verify_service;

pub async fn run(args: SessionsArgs) -> Result<i32, String> {
    let verification = verify_service(&args.base_url, None, &args.accepted_composes, false).await?;
    if !args.json {
        println!("== service verification: {} ==", verification.base_url);
        print!("{}", verification.transcript.render_human(false));
        println!();
    }
    if !verification.transcript.verified() {
        return Err("service verification failed; not auditing sessions (fail closed)".to_string());
    }
    if verification.report.service_capabilities.serving == "direct" {
        println!(
            "direct service (spec 4.1): no upstream hop, so there are no attested sessions to \
             pin — the spec 9.1 verification above is the whole prevention layer"
        );
        return Ok(0);
    }

    let audited = audit_current_sessions(
        &verification.client,
        &verification.base_url,
        args.model.as_deref(),
        &args.require_claims,
    )
    .await?;

    if args.json {
        let sessions: Vec<Value> = audited.iter().map(AuditedSession::to_json).collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "sessions": sessions }))
                .map_err(|e| e.to_string())?
        );
    } else {
        if audited.is_empty() {
            println!("no current attested sessions listed");
        }
        for session in &audited {
            println!("{}", session.render());
        }
        let accepted = audited.iter().filter(|s| s.accepted()).count();
        println!();
        println!(
            "{accepted} of {} session(s) accepted{}",
            audited.len(),
            if args.require_claims.is_empty() {
                String::new()
            } else {
                format!(
                    " under policy {}",
                    args.require_claims
                        .iter()
                        .map(RequiredClaim::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        );
    }

    // Fail closed: every listed session must audit clean, and a policy that
    // accepts nothing leaves the caller nothing to pin.
    let all_integral = audited.iter().all(|s| s.integrity_ok());
    let any_accepted = audited.iter().any(|s| s.accepted());
    Ok(
        if all_integral && (args.require_claims.is_empty() || any_accepted) {
            0
        } else {
            1
        },
    )
}

/// One listed session with its spec 9.2 audit outcome.
pub struct AuditedSession {
    pub session_id: String,
    /// `Err` when the full record could not be fetched or audited at all.
    pub audit: Result<SessionAudit, String>,
    /// Required claims the record does not satisfy (spec 9.2(3)).
    pub unmet: Vec<String>,
}

impl AuditedSession {
    pub fn integrity_ok(&self) -> bool {
        self.audit.as_ref().is_ok_and(SessionAudit::integrity_ok)
    }

    pub fn accepted(&self) -> bool {
        self.integrity_ok() && self.unmet.is_empty()
    }

    fn to_json(&self) -> Value {
        json!({
            "session_id": self.session_id,
            "accepted": self.accepted(),
            "integrity_ok": self.integrity_ok(),
            "unmet_claims": self.unmet,
            "error": self.audit.as_ref().err(),
            "record": self.audit.as_ref().ok().map(|audit| &audit.record),
        })
    }

    fn render(&self) -> String {
        let audit = match &self.audit {
            Err(e) => return format!("REJECTED {}: {e}", self.session_id),
            Ok(audit) => audit,
        };
        let record = &audit.record;
        let field = |key: &str| {
            record
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string()
        };
        let window = format!(
            "{}..{}",
            record
                .get("established_at")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            record
                .get("expires_at")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
        let claims: Vec<String> = record
            .get("claims")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(name, _)| name.as_str() != "extra")
            .filter_map(|(name, claim)| {
                let status = claim.get("status").and_then(Value::as_str)?;
                if status == "unknown" {
                    return None;
                }
                Some(match claim.get("source").and_then(Value::as_str) {
                    Some(source) => format!("{name} {status} ({source})"),
                    None => format!("{name} {status}"),
                })
            })
            .collect();
        let verdict = if self.accepted() {
            "ACCEPTED".to_string()
        } else if !self.integrity_ok() {
            format!(
                "REJECTED (integrity: {})",
                [
                    (!audit.version_ok).then_some("api_version"),
                    (!audit.id_matches).then_some("id mismatch"),
                    (!audit.in_window).then_some("outside validity window"),
                    audit.evidence.as_ref().err().map(|_| "evidence"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ")
            )
        } else {
            format!("REJECTED (unmet claims: {})", self.unmet.join(", "))
        };
        format!(
            "{verdict} {}\n  upstream={} verifier={} valid {window}\n  claims: {}",
            self.session_id,
            field("upstream_name"),
            field("verifier_id"),
            if claims.is_empty() {
                "none recorded".to_string()
            } else {
                claims.join(", ")
            },
        )
    }
}

/// List the service's current sessions and audit each full record (spec 9.2)
/// against `now` plus the claims policy. Shared with `aci serve
/// --require-claim`, which pins the accepted ids.
pub async fn audit_current_sessions(
    client: &AciClient,
    base_url: &str,
    model: Option<&str>,
    required_claims: &[RequiredClaim],
) -> Result<Vec<AuditedSession>, String> {
    let mut url = format!("{base_url}/v1/aci/sessions");
    if let Some(model) = model {
        url.push_str(&format!("?model={}", urlencoded(model)));
    }
    let listing = client.get(&url, None).await?;
    listing.error_for_status("session listing")?;
    let listing: Value = listing.json()?;
    let ids: Vec<String> = listing
        .get("sessions")
        .and_then(Value::as_array)
        .ok_or("session listing carries no sessions array")?
        .iter()
        .filter_map(|entry| {
            entry
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();

    let now = now_secs();
    let mut audited = Vec::with_capacity(ids.len());
    for session_id in ids {
        // The list entries drop the raw evidence data (§8.1), so only the
        // full served record can be audited.
        let audit = match client
            .get(&format!("{base_url}/v1/aci/sessions/{session_id}"), None)
            .await
            .and_then(|resp| {
                resp.error_for_status("session fetch")?;
                Ok(resp.body)
            }) {
            Ok(bytes) => crate::checks::audit_session_record(&bytes, &session_id, Some(now)),
            Err(e) => Err(e),
        };
        let unmet = audit
            .as_ref()
            .map(|audit| unmet_claims(&audit.record, required_claims))
            .unwrap_or_default();
        audited.push(AuditedSession {
            session_id,
            audit,
            unmet,
        });
    }
    Ok(audited)
}

/// Percent-encode the few bytes that would break a query value; model ids
/// are plain identifiers, so this stays minimal and reversible.
fn urlencoded(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
