//! Derive an `aci-service` verifier policy from an attestation report.
//!
//! Reads a report on stdin and prints the two values `accepted_subjects` and
//! `accepted_dstack_kms_root_public_keys` need: the RTMR3-measured app-id in
//! the `app-id:0x…` form the policy anchor compares against (§9.1(5)), and the
//! KMS root recovered from the receipt key's custody chain.
//!
//! This recovers what the report attests so an operator can pin it; the
//! gateway verifies the same chain on every request.

use std::io::{self, Read};

use k256::ecdsa::{RecoveryId, Signature as K256Signature, VerifyingKey as K256VerifyingKey};
use k256::EncodedPoint;
use private_ai_gateway::aci::types::AttestationReport;
use serde_json::Value;
use sha3::{Digest as Sha3Digest, Keccak256};

fn main() -> Result<(), String> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())?;
    let report: AttestationReport = serde_json::from_str(&input).map_err(|e| e.to_string())?;
    let app_id = app_id_from_report(&report)?;
    let root_public_key = kms_root_from_report(&report, &app_id)?;
    println!("subject=app-id:0x{}", hex::encode(&app_id));
    println!("app_id={}", hex::encode(&app_id));
    println!("kms_root_public_key={root_public_key}");
    Ok(())
}

fn app_id_from_report(report: &AttestationReport) -> Result<Vec<u8>, String> {
    let event_log = report
        .attestation
        .evidence
        .get("event_log")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing event_log".to_string())?;
    let events: Vec<Value> = serde_json::from_str(event_log).map_err(|e| e.to_string())?;
    let app_id = events
        .iter()
        .take_while(|event| {
            !(event.get("imr").and_then(Value::as_u64) == Some(3)
                && event.get("event").and_then(Value::as_str) == Some("system-ready"))
        })
        .find(|event| {
            event.get("imr").and_then(Value::as_u64) == Some(3)
                && event.get("event").and_then(Value::as_str) == Some("app-id")
        })
        .ok_or_else(|| "missing app-id event".to_string())?;
    decode_hex(
        app_id
            .get("event_payload")
            .and_then(Value::as_str)
            .ok_or_else(|| "app-id event missing event_payload".to_string())?,
    )
}

fn kms_root_from_report(report: &AttestationReport, app_id: &[u8]) -> Result<String, String> {
    let key_custody = report
        .attestation
        .evidence
        .get("key_custody")
        .ok_or_else(|| "missing key_custody".to_string())?;
    if key_custody.get("provider").and_then(Value::as_str) != Some("dstack-kms") {
        return Err("key_custody provider is not dstack-kms".to_string());
    }
    let keys = key_custody
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| "missing key_custody keys".to_string())?;
    let receipt = keys
        .iter()
        .find(|key| key.get("role").and_then(Value::as_str) == Some("receipt"))
        .ok_or_else(|| "missing receipt key custody".to_string())?;
    let public_key = field(receipt, "public_key")?;
    let keyset: Value = report.attestation.workload_keyset.clone();
    let listed = keyset
        .get("receipt_signing_keys")
        .and_then(Value::as_array)
        .ok_or_else(|| "keyset lists no receipt_signing_keys".to_string())?
        .iter()
        .any(|key| key.get("public_key").and_then(Value::as_str) == Some(public_key));
    if !listed {
        return Err("receipt key custody is not for a key the keyset lists".to_string());
    }
    let purpose = field(receipt, "purpose")?;
    let kms_public_key = field(receipt, "kms_public_key")?;
    let signature_chain = receipt
        .get("signature_chain")
        .and_then(Value::as_array)
        .ok_or_else(|| "receipt key custody missing signature_chain".to_string())?;
    if signature_chain.len() != 2 {
        return Err(format!(
            "signature_chain must contain 2 signatures, got {}",
            signature_chain.len()
        ));
    }
    let purpose_signature = decode_hex(
        signature_chain[0]
            .as_str()
            .ok_or_else(|| "signature_chain[0] is not a string".to_string())?,
    )?;
    let app_signature = decode_hex(
        signature_chain[1]
            .as_str()
            .ok_or_else(|| "signature_chain[1] is not a string".to_string())?,
    )?;

    let purpose_message = format!(
        "{purpose}:{}",
        compressed_k256_public_key_hex(kms_public_key)?
    );
    let app_public_key = recover_k256_public_key(purpose_message.as_bytes(), &purpose_signature)?;
    let root_message = [
        b"dstack-kms-issued".as_slice(),
        b":",
        app_id,
        &app_public_key.to_sec1_bytes(),
    ]
    .concat();
    let root_public_key = recover_k256_public_key(&root_message, &app_signature)?;
    Ok(hex::encode(root_public_key.to_sec1_bytes()))
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("receipt key custody missing {key}"))
}

fn compressed_k256_public_key_hex(public_key_hex: &str) -> Result<String, String> {
    let bytes = decode_hex(public_key_hex)?;
    let point = EncodedPoint::from_bytes(&bytes).map_err(|e| e.to_string())?;
    let key = K256VerifyingKey::from_encoded_point(&point).map_err(|e| e.to_string())?;
    Ok(hex::encode(key.to_sec1_bytes()))
}

fn recover_k256_public_key(message: &[u8], signature: &[u8]) -> Result<K256VerifyingKey, String> {
    if signature.len() != 65 {
        return Err(format!(
            "recoverable signature must be 65 bytes, got {}",
            signature.len()
        ));
    }
    let sig = K256Signature::from_slice(&signature[..64]).map_err(|e| e.to_string())?;
    let recovery_id = RecoveryId::from_byte(normalize_recovery_byte(signature[64]))
        .ok_or_else(|| "invalid recovery id".to_string())?;
    let digest = Keccak256::new_with_prefix(message);
    K256VerifyingKey::recover_from_digest(digest, &sig, recovery_id).map_err(|e| e.to_string())
}

fn normalize_recovery_byte(byte: u8) -> u8 {
    if byte >= 27 {
        byte - 27
    } else {
        byte
    }
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    hex::decode(value.strip_prefix("0x").unwrap_or(value)).map_err(|e| e.to_string())
}
