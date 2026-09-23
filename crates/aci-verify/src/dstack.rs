//! dstack event-log replay and RTMR3-bound measurement helpers.

use serde_json::Value;
use sha2::{Digest, Sha256, Sha384};

use crate::decode_hex;

const DSTACK_RUNTIME_EVENT_TYPE: u32 = 0x08000001;

/// Wire representation of dstack event evidence.
#[derive(
    Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Deserialize, serde::Serialize,
)]
pub struct DstackEventLog {
    pub imr: u32,
    pub event_type: u32,
    pub digest: String,
    pub event: String,
    pub event_payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AppComposeError {
    #[error("missing dstack app_compose evidence")]
    Missing,
    #[error("dstack app_compose preimage does not match the RTMR3-bound compose hash")]
    HashMismatch,
}

/// Replay the dstack event log to RTMR3 and require it to match the quote.
pub fn verify_dstack_event_log(
    evidence: &Value,
    report: &dcap_qvl::quote::Report,
) -> Result<Vec<DstackEventLog>, String> {
    let event_log = evidence
        .get("event_log")
        .and_then(Value::as_str)
        .ok_or("missing dstack event_log evidence")?;
    let events = serde_json::from_str::<Vec<DstackEventLog>>(event_log)
        .map_err(|error| format!("invalid dstack event_log evidence: {error}"))?;
    let rtmr3 = replay_dstack_rtmr(&events, 3)?;
    let quote_rtmr3 =
        dcap_rtmr3(report).ok_or("dstack event log verification requires a TDX quote")?;
    if rtmr3.as_slice() != quote_rtmr3 {
        return Err("dstack event_log RTMR3 does not match verified quote".to_string());
    }
    Ok(events)
}

/// Find the single pre-`system-ready` dstack runtime event named `event_name`.
pub fn dstack_rtmr3_event<'a>(
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

/// Verify `app_compose` against the RTMR3-bound compose-hash event and return
/// the measured hash as lowercase hex.
pub fn verify_dstack_compose_measurement(
    evidence: &Value,
    events: &[DstackEventLog],
) -> Result<String, String> {
    let measured = dstack_rtmr3_event(events, "compose-hash")
        .map_err(|error| format!("dstack event log rejected: {error}"))?
        .ok_or_else(|| "verified event log carries no compose-hash event".to_string())?;
    let measured_hash: [u8; 32] = decode_hex(&measured.event_payload)?
        .as_slice()
        .try_into()
        .map_err(|_| "compose-hash event must contain 32 bytes".to_string())?;
    verify_dstack_app_compose(evidence, &measured_hash).map_err(|error| error.to_string())?;
    Ok(hex::encode(measured_hash))
}

/// Return the RTMR3-measured app-id used by custody policies.
pub fn dstack_app_id(events: &[DstackEventLog]) -> Result<Vec<u8>, String> {
    let event = dstack_rtmr3_event(events, "app-id")
        .map_err(|error| format!("dstack event log rejected: {error}"))?
        .ok_or_else(|| "verified event log carries no app-id event".to_string())?;
    decode_hex(&event.event_payload)
}

/// Verify that `app_compose` is the preimage of the measured compose hash.
pub fn verify_dstack_app_compose(
    evidence: &Value,
    measured_compose_hash: &[u8; 32],
) -> Result<(), AppComposeError> {
    let app_compose = evidence
        .get("app_compose")
        .and_then(Value::as_str)
        .ok_or(AppComposeError::Missing)?;
    let actual_compose_hash: [u8; 32] = Sha256::digest(app_compose.as_bytes()).into();
    if &actual_compose_hash != measured_compose_hash {
        return Err(AppComposeError::HashMismatch);
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
            dstack_rtmr3_event(&[disguised_firmware_event], "compose-hash")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_duplicate_semantic_events() {
        let compose_hash = runtime_event("compose-hash", &[0x44; 32]);
        let error =
            dstack_rtmr3_event(&[compose_hash.clone(), compose_hash], "compose-hash").unwrap_err();

        assert_eq!(
            error,
            "invalid dstack event_log evidence: multiple pre-system-ready compose-hash events"
        );
    }
}
