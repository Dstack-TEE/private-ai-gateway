use aci_verify::dstack::{verify_dstack_compose_measurement, verify_dstack_event_log};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[test]
fn upstream_event_vectors_bind_fields_and_replay() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../clients/verifier-ts/test/fixtures/dstack_event_vectors.json"
    ))
    .unwrap();
    for vector in fixture["vectors"].as_array().unwrap() {
        let evidence = json!({
            "event_log": serde_json::to_string(&vector["events"]).unwrap(),
            "app_compose": vector["app_compose"],
        });
        let rtmr3: [u8; 48] = hex::decode(vector["rtmr3"].as_str().unwrap())
            .unwrap()
            .try_into()
            .unwrap();
        let result = verify_dstack_event_log(&evidence, Some(&rtmr3))
            .and_then(|log| verify_dstack_compose_measurement(&evidence, &log));
        assert_eq!(
            result.is_ok(),
            vector["valid"].as_bool().unwrap(),
            "{}: {result:?}",
            vector["name"]
        );
        if let Ok(hash) = result {
            assert_eq!(hash, vector["compose_hash"].as_str().unwrap());
        }
    }
}

#[test]
fn recorded_quote_rejects_forged_compose_payload() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../clients/verifier-ts/test/fixtures/dstack_v1_attestation.json"
    ))
    .unwrap();
    let mut evidence = fixture["report"]["attestation"]["evidence"].clone();
    // This crate accepts an already-verified RTMR3. The TS regression also
    // checks this recorded quote's signatures with its recorded collateral.
    let quote = hex::decode(evidence["quote"].as_str().unwrap()).unwrap();
    let rtmr3: [u8; 48] = quote[520..568].try_into().unwrap();
    let log = verify_dstack_event_log(&evidence, Some(&rtmr3)).unwrap();
    verify_dstack_compose_measurement(&evidence, &log).unwrap();

    let forged = format!("{}\n", evidence["app_compose"].as_str().unwrap());
    let mut events: Value = serde_json::from_str(evidence["event_log"].as_str().unwrap()).unwrap();
    let compose = events
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|e| e["imr"] == 3 && e["event"] == "compose-hash")
        .unwrap();
    compose["event_payload"] = json!(hex::encode(Sha256::digest(forged.as_bytes())));
    evidence["app_compose"] = json!(forged);
    evidence["event_log"] = json!(serde_json::to_string(&events).unwrap());
    assert!(verify_dstack_event_log(&evidence, Some(&rtmr3))
        .unwrap_err()
        .contains("digest does not match"));
}
