//! dstack event-log replay used by the client's provenance appraisal.

use serde_json::Value;
use sha2::{Digest, Sha256, Sha384};

use super::decode_hex;

const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x08000001;

/// Wire representation of the dstack event evidence consumed by verification.
/// Verification is platform-neutral and does not depend on the Unix client SDK.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
pub struct DstackEventLog {
    pub imr: u32,
    pub event_type: u32,
    pub digest: String,
    pub event: String,
    pub event_payload: String,
}

/// Replay the dstack event log to RTMR3 and require it to match the quote,
/// returning the verified events. Private AI Proxy reuses this for its own
/// §9.1(4) compose check, so failures are plain strings rather than this
/// module's provider-verifier error type.
pub fn verify_dstack_event_log(
    evidence: &Value,
    report: &dcap_qvl::quote::Report,
) -> Result<Vec<DstackEventLog>, String> {
    let event_log = evidence
        .get("event_log")
        .and_then(Value::as_str)
        .ok_or("missing dstack event_log evidence")?;
    let events = serde_json::from_str::<Vec<DstackEventLog>>(event_log)
        .map_err(|e| format!("invalid dstack event_log evidence: {e}"))?;
    let rtmr3 = replay_dstack_rtmr(&events, 3)?;
    let quote_rtmr3 =
        dcap_rtmr3(report).ok_or("dstack event log verification requires a TDX quote")?;
    if rtmr3.as_slice() != quote_rtmr3 {
        return Err("dstack event_log RTMR3 does not match verified quote".to_string());
    }
    Ok(events)
}

/// The single pre-`system-ready` dstack runtime event named `event_name`
/// (`None` when the verified log carries none). A log carrying more than one
/// is the tampering shape this lookup exists to catch, so it is an error,
/// never a silent "absent".
pub fn dstack_rtmr3_event<'a>(
    events: &'a [DstackEventLog],
    event_name: &str,
) -> Result<Option<&'a DstackEventLog>, String> {
    runtime_event_before_system_ready(events, event_name).map_err(|e| e.to_string())
}

fn runtime_event_before_system_ready<'a>(
    events: &'a [DstackEventLog],
    event_name: &str,
) -> Result<Option<&'a DstackEventLog>, String> {
    let mut matches = events
        .iter()
        .take_while(|event| {
            !(event.imr == 3
                && event.event_type == DSTACK_RUNTIME_EVENT_TYPE
                && event.event == "system-ready")
        })
        .filter(|event| {
            event.imr == 3
                && event.event_type == DSTACK_RUNTIME_EVENT_TYPE
                && event.event == event_name
        });
    let event = matches.next();
    if matches.next().is_some() {
        return Err(format!(
            "invalid dstack event_log evidence: multiple pre-system-ready {event_name} events"
        ));
    }
    Ok(event)
}

/// The RTMR3-measured compose hash that `app_compose` reproduces (§9.1(4)).
///
/// Returns the measured hash as lowercase hex, so a caller can report or pin
/// it.
pub(super) fn verify_dstack_compose_measurement(
    evidence: &Value,
    events: &[DstackEventLog],
) -> Result<String, String> {
    let measured = dstack_rtmr3_event(events, "compose-hash")
        .map_err(|e| format!("dstack event log rejected: {e}"))?
        .ok_or_else(|| "verified event log carries no compose-hash event".to_string())?;
    let measured_hash: [u8; 32] = decode_hex(&measured.event_payload)?
        .as_slice()
        .try_into()
        .map_err(|_| "compose-hash event must contain 32 bytes".to_string())?;
    verify_dstack_app_compose(evidence, &measured_hash)?;
    Ok(hex::encode(measured_hash))
}

/// Verify that `app_compose` is the preimage of the compose measurement
/// bound into RTMR3 by the verified event log.
pub(super) fn verify_dstack_app_compose(
    evidence: &Value,
    measured_compose_hash: &[u8; 32],
) -> Result<(), String> {
    let app_compose = evidence
        .get("app_compose")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing dstack app_compose evidence".to_string())?;
    let actual_compose_hash: [u8; 32] = Sha256::digest(app_compose.as_bytes()).into();
    if &actual_compose_hash != measured_compose_hash {
        return Err(
            "dstack app_compose preimage does not match the RTMR3-bound compose hash".into(),
        );
    }
    Ok(())
}

fn replay_dstack_rtmr(events: &[DstackEventLog], imr: u32) -> Result<[u8; 48], String> {
    let mut mr = vec![0u8; 48];
    for event in events.iter().filter(|event| event.imr == imr) {
        let mut digest = dstack_event_digest(event)?;
        if digest.len() < 48 {
            digest.resize(48, 0);
        }
        mr.extend_from_slice(&digest);
        mr = Sha384::digest(&mr).to_vec();
    }
    mr.as_slice()
        .try_into()
        .map_err(|_| "invalid dstack event_log evidence: replayed RTMR is not 48 bytes".into())
}

fn dstack_event_digest(event: &DstackEventLog) -> Result<Vec<u8>, String> {
    if event.event_type != DSTACK_RUNTIME_EVENT_TYPE {
        return decode_hex(&event.digest)
            .map_err(|error| format!("invalid dstack event_log evidence: {error}"));
    }

    let payload = decode_hex(&event.event_payload)
        .map_err(|error| format!("invalid dstack event_log evidence: {error}"))?;
    let mut hasher = Sha384::new();
    hasher.update(event.event_type.to_ne_bytes());
    hasher.update(b":");
    hasher.update(event.event.as_bytes());
    hasher.update(b":");
    hasher.update(payload);
    Ok(hasher.finalize().to_vec())
}

fn dcap_rtmr3(report: &dcap_qvl::quote::Report) -> Option<&[u8; 48]> {
    match report {
        dcap_qvl::quote::Report::TD10(report) => Some(&report.rt_mr3),
        dcap_qvl::quote::Report::TD15(report) => Some(&report.base.rt_mr3),
        dcap_qvl::quote::Report::SgxEnclave(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_event(event: &str, payload: &[u8]) -> DstackEventLog {
        let mut event = DstackEventLog {
            imr: 3,
            event_type: DSTACK_RUNTIME_EVENT_TYPE,
            digest: String::new(),
            event: event.to_string(),
            event_payload: hex::encode(payload),
        };
        event.digest = hex::encode(dstack_event_digest(&event).unwrap());
        event
    }

    #[test]
    fn replay_recomputes_runtime_event_digest_from_semantic_fields() {
        let measured = runtime_event("app-id", &[0x11; 20]);
        let expected_rtmr = replay_dstack_rtmr(std::slice::from_ref(&measured), 3).unwrap();

        let mut tampered = runtime_event("compose-hash", &[0x22; 32]);
        tampered.digest = measured.digest;
        let tampered_rtmr = replay_dstack_rtmr(&[tampered], 3).unwrap();

        assert_ne!(tampered_rtmr, expected_rtmr);
    }

    #[test]
    fn semantic_events_must_be_dstack_runtime_events() {
        let disguised_firmware_event = DstackEventLog {
            imr: 3,
            event_type: 0,
            digest: hex::encode([0x33; 48]),
            event: "compose-hash".to_string(),
            event_payload: hex::encode([0x44; 32]),
        };

        assert!(
            runtime_event_before_system_ready(&[disguised_firmware_event], "compose-hash")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_duplicate_semantic_events() {
        let compose_hash = runtime_event("compose-hash", &[0x44; 32]);
        let err = runtime_event_before_system_ready(
            &[compose_hash.clone(), compose_hash],
            "compose-hash",
        )
        .unwrap_err()
        .to_string();

        assert_eq!(
            err,
            "invalid dstack event_log evidence: multiple pre-system-ready compose-hash events"
        );
    }
}
