//! Digest and canonicalization helpers shared across the ACI surface.
//!
//! Digest fields are `"sha256:" || lowercase-hex` over the exact bytes
//! named (Appendix A). Receipts and sessions hash and sign the JCS form of
//! the parsed document, so the served encoding is free (§7.2, §8).

use serde_json::Value;
use sha2::{Digest, Sha256};

/// `"sha256:" || hex(sha256(payload))` — the digest-field format.
pub fn sha256_hex(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(payload)))
}

/// Bare lowercase hex of sha256 — the id format (e.g. session ids, §8).
pub fn sha256_bare_hex(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

/// Raw 32-byte SHA-256 of `payload`.
pub fn sha256_raw(payload: &[u8]) -> [u8; 32] {
    Sha256::digest(payload).into()
}

/// A value violates the ACI signed-object constraints (Appendix A).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JcsError {
    #[error("non-integer JSON number {0} in an ACI JSON document (Appendix A)")]
    NonIntegerNumber(String),
    #[error("non-ASCII field name {0:?} in an ACI JSON document (Appendix A)")]
    NonAsciiFieldName(String),
}

/// JCS (RFC 8785) bytes of a JSON value under the ACI artifact constraints:
/// ASCII member names and integer numbers (Appendix A). Under those
/// constraints JCS is exactly compact serialization with lexicographically
/// sorted field names, which is what this produces — and the constraints
/// are enforced, so a value that would canonicalize differently under a
/// full RFC 8785 implementation (float formatting, UTF-16 name order) is
/// rejected instead of hashed divergently.
pub fn jcs_bytes(value: &Value) -> Result<Vec<u8>, JcsError> {
    let sorted = sorted(value)?;
    Ok(serde_json::to_vec(&sorted).expect("JSON value serializes"))
}

fn sorted(value: &Value) -> Result<Value, JcsError> {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_unstable();
            let mut out = serde_json::Map::new();
            for key in keys {
                if !key.is_ascii() {
                    return Err(JcsError::NonAsciiFieldName(key.clone()));
                }
                out.insert(key.clone(), sorted(&map[key.as_str()])?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => Ok(Value::Array(
            items.iter().map(sorted).collect::<Result<_, _>>()?,
        )),
        Value::Number(n) if !n.is_i64() && !n.is_u64() => {
            Err(JcsError::NonIntegerNumber(n.to_string()))
        }
        other => Ok(other.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jcs_rejects_floats_and_non_ascii_member_names() {
        use serde_json::json;
        assert!(jcs_bytes(&json!({"a": 1, "b": [2, {"c": "x"}]})).is_ok());
        assert_eq!(
            jcs_bytes(&json!({"a": 2.5})),
            Err(JcsError::NonIntegerNumber("2.5".to_string()))
        );
        // 2.0 parses as a float even though it is integral in value.
        assert!(jcs_bytes(&serde_json::from_str::<Value>(r#"{"a": 2.0}"#).unwrap()).is_err());
        assert_eq!(
            jcs_bytes(&json!({"héx": 1})),
            Err(JcsError::NonAsciiFieldName("héx".to_string()))
        );
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
