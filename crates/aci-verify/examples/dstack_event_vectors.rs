//! Regenerate the shared fixture with the pinned upstream cc-eventlog:
//! cargo run -q -p aci-verify --example dstack_event_vectors > clients/verifier-ts/test/fixtures/dstack_event_vectors.json
use cc_eventlog::{EventLogVersion, RuntimeEvent, TdxEvent};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn vector(name: &str, versions: &[EventLogVersion]) -> Value {
    let compose = "services:\n  gateway:\n    image: demo\n";
    let compose_hash = Sha256::digest(compose.as_bytes()).to_vec();
    let fields = [
        ("app-id", vec![0x11; 20]),
        ("compose-hash", compose_hash.clone()),
        // Upstream accepts arbitrary names; cover UTF-8, delimiters and JCS escapes.
        ("custom:event-测试-🦀\"\\\n\u{1}", vec![0xab, 0xcd, 0xff]),
        // Upstream find_event returns the first boot-time match.
        ("compose-hash", vec![0x22; 32]),
        ("system-ready", vec![]),
        ("compose-hash", vec![0x33; 32]),
    ];
    let runtime: Vec<_> = fields
        .into_iter()
        .enumerate()
        .map(|(i, (event, payload))| {
            RuntimeEvent::new(event.into(), payload, versions[i % versions.len()])
        })
        .collect();
    let rtmr3 = cc_eventlog::replay_events::<ez_hash::Sha384>(&runtime, None);
    let mut events: Vec<TdxEvent> = runtime.into_iter().map(Into::into).collect();
    cc_eventlog::tdx::fill_v2_preimages(&mut events);
    json!({"name": name, "events": events, "rtmr3": hex::encode(rtmr3),
        "app_compose": compose, "compose_hash": hex::encode(compose_hash), "valid": true})
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let v1 = vector("v1", &[EventLogVersion::V1]);
    let v2 = vector("v2", &[EventLogVersion::V2]);
    let mixed = vector("mixed-v1-v2", &[EventLogVersion::V1, EventLogVersion::V2]);
    let mut vectors = vec![v1.clone(), v2.clone(), mixed];
    for (name, field, value) in [
        ("v1-forged-payload", "event_payload", json!("ff".repeat(32))),
        ("v1-forged-digest", "digest", json!("00".repeat(48))),
        ("non-runtime-rtmr3", "event_type", json!(0)),
    ] {
        let mut bad = v1.clone();
        bad["name"] = json!(name);
        bad["valid"] = json!(false);
        bad["events"][1][field] = value;
        vectors.push(bad);
    }
    // Recompute a forged payload's digest with upstream: digest validation
    // passes, but the quote must still reject its changed replay.
    let mut rebound = v1.clone();
    rebound["name"] = json!("v1-consistent-forgery-wrong-quote");
    rebound["valid"] = json!(false);
    rebound["events"][1] = serde_json::to_value(TdxEvent::from(RuntimeEvent::new(
        "compose-hash".into(),
        vec![0xff; 32],
        EventLogVersion::V1,
    )))
    .unwrap();
    vectors.push(rebound);
    for (name, preimage) in [
        ("v2-missing-preimage", Value::Null),
        ("v2-malformed-preimage", json!("not-hex")),
        ("v2-wrong-preimage", json!("00")),
    ] {
        let mut bad = v2.clone();
        bad["name"] = json!(name);
        bad["valid"] = json!(false);
        bad["events"][1]["preimage"] = preimage;
        vectors.push(bad);
    }
    // Same JSON meaning, different bytes: even a matching advertised digest
    // must not make a noncanonical V2 preimage valid.
    let mut bad = v2;
    bad["name"] = json!("v2-noncanonical-preimage");
    bad["valid"] = json!(false);
    let supplied = format!(
        "{{\"type\":134217729,\"name\":\"compose-hash\",\"payload\":\"{}\"}}",
        bad["compose_hash"].as_str().unwrap()
    );
    bad["events"][1]["preimage"] = json!(hex::encode(supplied.as_bytes()));
    bad["events"][1]["digest"] = json!(hex::encode(sha2::Sha384::digest(supplied.as_bytes())));
    vectors.push(bad);
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "upstream": "Dstack-TEE/dstack@verifier-v0.6.3 (dad8326f89a76e60e5d4b61c424a3329c92f60e9)",
            "vectors": vectors,
        }))?
    );
    Ok(())
}
