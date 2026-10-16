use aci_verify::dstack::{
    verify_dstack_app_compose, verify_dstack_compose_measurement, verify_dstack_event_log,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256, Sha384};

#[derive(Deserialize)]
struct PolicyCase {
    name: String,
    valid: bool,
    replacements: Option<Vec<(String, String)>>,
    app_compose_json: Option<String>,
    unknown_field_bytes: Option<usize>,
    nested_arrays: Option<usize>,
    extra_boot_events: Option<usize>,
    pad_log_to: Option<usize>,
    synthetic_tail: Option<Value>,
}

#[test]
fn same_recorded_evidence_and_policy_cases_as_typescript() {
    let recorded: Value = serde_json::from_str(include_str!(
        "../../../clients/verifier-ts/test/fixtures/dstack_v1_attestation.json"
    ))
    .unwrap();
    let cases: Vec<PolicyCase> = serde_json::from_str(include_str!(
        "../../../clients/verifier-ts/test/fixtures/dstack_policy_cases.json"
    ))
    .unwrap();
    for case in cases {
        let result = run_case(&recorded, &case);
        assert_eq!(result.is_ok(), case.valid, "{}: {result:?}", case.name);
    }
}

fn run_case(recorded: &Value, case: &PolicyCase) -> Result<String, String> {
    let mut evidence = recorded["report"]["attestation"]["evidence"].clone();
    let mut log = evidence["event_log"].as_str().unwrap().to_string();
    for (from, to) in case.replacements.iter().flatten() {
        assert!(log.contains(from));
        log = log.replacen(from, to, 1);
    }
    if let Some(app) = &case.app_compose_json {
        // serde_json rejects lone surrogates at the outer evidence boundary.
        evidence["app_compose"] = serde_json::from_str::<Value>(app).map_err(|e| e.to_string())?;
    }
    if let Some(bytes) = case.unknown_field_bytes {
        log = log.replacen(
            '{',
            &format!("{{\"extra\":{},", json!("a".repeat(bytes))),
            1,
        );
    }
    if let Some(depth) = case.nested_arrays {
        log = log.replacen(
            '{',
            &format!("{{\"extra\":{}0{},", "[".repeat(depth), "]".repeat(depth)),
            1,
        );
    }
    if let Some(count) = case.extra_boot_events {
        let events: Vec<Value> = serde_json::from_str(&log).unwrap();
        let mut padded = vec![events[0].clone(); count];
        padded.extend(events);
        log = serde_json::to_string(&padded).unwrap();
    }
    if let Some(bytes) = case.pad_log_to {
        log.push_str(&" ".repeat(bytes - log.len()));
    }
    let mut quote = hex::decode(evidence["quote"].as_str().unwrap()).unwrap();
    if let Some(tail) = &case.synthetic_tail {
        // Explicitly synthetic quote fields for name-policy vectors;
        // these cases test event replay, not hardware quote authentication.
        let mut events: Vec<Value> = serde_json::from_str(&log).unwrap();
        events.push(tail.clone());
        let mut mr = vec![0u8; 48];
        for event in events.iter().filter(|event| event["imr"] == 3) {
            mr.extend(hex::decode(event["digest"].as_str().unwrap()).unwrap());
            mr = Sha384::digest(&mr).to_vec();
        }
        quote[520..568].copy_from_slice(&mr);
        log = serde_json::to_string(&events).unwrap();
    }
    evidence["event_log"] = Value::String(log);
    let rtmr3: [u8; 48] = quote[520..568].try_into().unwrap();
    let verified = verify_dstack_event_log(&evidence, Some(&rtmr3))?;
    verify_dstack_compose_measurement(&evidence, &verified)
}

#[test]
fn compose_utf8_byte_limit_is_checked_before_hashing() {
    let app = "é".repeat(4 * 1024 * 1024 / 2);
    let hash: [u8; 32] = Sha256::digest(app.as_bytes()).into();
    assert!(verify_dstack_app_compose(&json!({"app_compose":app}), &hash).is_ok());
    let over_limit = format!("{app}a");
    let error = verify_dstack_app_compose(&json!({"app_compose":over_limit}), &hash).unwrap_err();
    assert!(error.contains("maximum byte size"));
}
