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
    println!("workload_id={}", report.workload_id);
    println!("app_id={}", hex::encode(app_id));
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
    let identity = keys
        .iter()
        .find(|key| key.get("role").and_then(Value::as_str) == Some("identity"))
        .ok_or_else(|| "missing identity key custody".to_string())?;
    let public_key = identity
        .get("public_key")
        .and_then(Value::as_str)
        .ok_or_else(|| "identity key custody missing public_key".to_string())?;
    let expected_public_key = &report
        .attestation
        .workload_keyset
        .workload_identity
        .public_key
        .public_key_hex;
    if public_key != expected_public_key {
        return Err("identity key custody does not match report workload identity".to_string());
    }
    let purpose = identity
        .get("purpose")
        .and_then(Value::as_str)
        .ok_or_else(|| "identity key custody missing purpose".to_string())?;
    let signature_chain = identity
        .get("signature_chain")
        .and_then(Value::as_array)
        .ok_or_else(|| "identity key custody missing signature_chain".to_string())?;
    if signature_chain.len() != 2 {
        return Err(format!(
            "identity key custody signature_chain must contain 2 signatures, got {}",
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

    let purpose_message = format!("{purpose}:{}", compressed_k256_public_key_hex(public_key)?);
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

fn recover_k256_public_key(message: &[u8], signature: &[u8]) -> Result<K256VerifyingKey, String> {
    if signature.len() != 65 {
        return Err(format!(
            "recoverable secp256k1 signature must be 65 bytes, got {}",
            signature.len()
        ));
    }
    let mut recovery_byte = signature[64];
    if (27..=30).contains(&recovery_byte) {
        recovery_byte -= 27;
    }
    let recid = RecoveryId::from_byte(recovery_byte)
        .ok_or_else(|| format!("invalid recovery id: {}", signature[64]))?;
    let sig = K256Signature::from_slice(&signature[..64])
        .map_err(|e| format!("invalid secp256k1 signature: {e}"))?;
    let digest = Keccak256::new_with_prefix(message);
    K256VerifyingKey::recover_from_digest(digest, &sig, recid)
        .map_err(|e| format!("secp256k1 public key recovery failed: {e}"))
}

fn compressed_k256_public_key_hex(public_key_hex: &str) -> Result<String, String> {
    let public_key = decode_hex(public_key_hex)?;
    let point = EncodedPoint::from_bytes(public_key)
        .map_err(|e| format!("invalid secp256k1 public key: {e}"))?;
    let key = K256VerifyingKey::from_encoded_point(&point)
        .map_err(|e| format!("invalid secp256k1 public key: {e}"))?;
    Ok(hex::encode(key.to_sec1_bytes()))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    hex::decode(value.strip_prefix("0x").unwrap_or(value)).map_err(|e| e.to_string())
}
