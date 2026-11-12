//! Snapshot normalization stays in the harness; production uses real randomness
//! and wall clocks. Verify cryptography and raw-byte hashes before projecting.
use serde_json::{json, Value};

use crate::aci::digest::sha256_hex;

pub(super) fn normalize_wire(wire: &str, encrypted: bool) -> String {
    fn payload(text: &str, encrypted: bool) -> String {
        let Ok(mut value) = serde_json::from_str::<Value>(text) else {
            return text.to_string();
        };
        let original = value.clone();
        for pointer in ["/created", "/created_at", "/response/created_at"] {
            if let Some(timestamp) = value.pointer_mut(pointer) {
                assert!(timestamp.is_u64() || timestamp == "<timestamp>");
                *timestamp = "<timestamp>".into();
            }
        }
        if encrypted {
            for choice in value["choices"].as_array_mut().unwrap() {
                for field in ["content", "reasoning", "reasoning_content"] {
                    if let Some(ciphertext) = choice["delta"].get_mut(field) {
                        if !ciphertext.is_null() {
                            *ciphertext = "<ciphertext>".into();
                        }
                    }
                }
            }
        }
        if value == original {
            return text.to_string();
        }
        // Only replace scalar values in compact generated payloads. Never hide
        // differences in JSON whitespace, ordering or SSE framing.
        assert_eq!(serde_json::to_string(&original).unwrap(), text);
        serde_json::to_string(&value).unwrap()
    }
    if serde_json::from_str::<Value>(wire).is_ok() {
        return payload(wire, encrypted);
    }
    wire.split_inclusive('\n')
        .map(|line| {
            let text = line.trim_end_matches(['\r', '\n']);
            let Some(data) = text.strip_prefix("data: ") else {
                return line.to_string();
            };
            format!("data: {}{}", payload(data, encrypted), &line[text.len()..])
        })
        .collect()
}

pub(super) fn normalize_snapshot(snapshot: &mut Value) {
    let wire = snapshot["wire"].as_str().unwrap();
    assert_eq!(snapshot["wire_hash"], sha256_hex(wire.as_bytes()));
    let encrypted = matches!(
        snapshot["name"].as_str(),
        Some("e2ee_chat" | "buffered_sse_e2ee")
    );
    let wire = normalize_wire(wire, encrypted);
    let hash = sha256_hex(wire.as_bytes());
    let source_hash = snapshot["source_hash"].clone();
    let wire_hash = snapshot["wire_hash"].clone();
    if let Some(receipt) = snapshot["receipt"].as_object_mut() {
        for event in receipt["event_log"].as_array_mut().unwrap() {
            match event["type"].as_str().unwrap() {
                "response.received" => assert_eq!(event["body_hash"], source_hash),
                "response.returned" => {
                    assert_eq!(event["body_hash"], wire_hash);
                    event["body_hash"] = hash.clone().into();
                }
                _ => {}
            }
        }
        receipt.insert("receipt_id".into(), "<receipt-id>".into());
        receipt.insert("signature".into(), "<signature>".into());
    }
    snapshot["wire"] = wire.into();
    snapshot["wire_hash"] = hash.into();
    if let Some(reports) = snapshot["reports"].as_array_mut() {
        for report in reports {
            let duration = &report["durationMs"];
            assert!(duration.is_u64() || duration == "<duration-ms>");
            if let Some(ttft) = report["ttftMs"].as_u64() {
                assert!(ttft <= duration.as_u64().unwrap());
                report["ttftMs"] = "<duration-ms>".into();
            }
            report["durationMs"] = "<duration-ms>".into();
        }
    }
}

pub(super) fn verify_e2ee_wire(wire: &str, source: &str) {
    let payloads = |wire: &str| {
        wire.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line.strip_prefix("data: ")?).ok())
            .collect::<Vec<_>>()
    };
    let source = payloads(source);
    let output = payloads(wire);
    assert_eq!(source.len(), output.len());
    let secret = crate::aci::e2ee::x25519_secret_key_from_bytes(&[0x37; 32]).unwrap();
    let mut fields = 0;
    for (source, output) in source.iter().zip(output) {
        for (position, choice) in output["choices"].as_array().unwrap().iter().enumerate() {
            let index = choice["index"].as_u64().unwrap_or(position as u64);
            for field in ["content", "reasoning", "reasoning_content"] {
                let Some(ciphertext) = choice["delta"][field].as_str() else {
                    continue;
                };
                let aad = crate::aci::digest::jcs_bytes(&json!({
                    "purpose":"aci.e2ee.response.v2",
                    "algo":crate::aci::e2ee::E2EE_ALGO_X25519_AESGCM,
                    "model":"fixture-model", "id":output["id"],
                    "field":format!("choices.{index}.delta.{field}"),
                    "nonce":"01".repeat(32), "ts":1_700_000_000,
                }))
                .unwrap();
                assert_eq!(
                    crate::aci::e2ee::decrypt_x25519_with_secret_key(&secret, ciphertext, &aad)
                        .unwrap(),
                    source["choices"][position]["delta"][field]
                        .as_str()
                        .unwrap()
                        .as_bytes(),
                );
                assert!(crate::aci::e2ee::decrypt_x25519_with_secret_key(
                    &secret,
                    ciphertext,
                    b"tampered AAD"
                )
                .is_err());
                fields += 1;
            }
        }
    }
    assert!(fields > 0);
}
