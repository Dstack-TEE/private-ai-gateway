//! Workload identity and attestation binding (ACI §4).
//!
//! The keyset is the unit of identity: it is serialized exactly once when
//! sealed, the digest is over those bytes, and the hardware quote binds the
//! digest through the fixed-byte attestation statement (§4.2). Verifiers
//! recompute the same chain from the served bytes; no canonicalization exists.

use serde_json::Value;

use super::digest;
use super::types::WorkloadKeyset;

/// Purpose tag embedded in the attestation statement (§4.2).
pub const REPORT_DATA_PURPOSE: &str = "aci.report_data.v1";

/// Maximum accepted nonce length (§4.2).
pub const MAX_NONCE_LEN: usize = 128;

#[derive(Debug, thiserror::Error)]
#[error("nonce must be 1-128 characters from [0-9A-Za-z_-]")]
pub struct InvalidNonce;

/// `"sha256:" || hex(sha256(keyset bytes))` over the exact serialized keyset.
pub fn workload_keyset_digest(keyset_bytes: &[u8]) -> String {
    digest::sha256_hex(keyset_bytes)
}

/// True when `nonce` is 1–128 characters from `[0-9A-Za-z_-]` (§4.2). The
/// charset keeps the statement template escape-free.
pub fn is_valid_nonce(nonce: &str) -> bool {
    !nonce.is_empty()
        && nonce.len() <= MAX_NONCE_LEN
        && nonce
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Build the exact statement bytes the TEE quote binds (§4.2):
///
/// ```text
/// {"keyset_digest":"sha256:<hex>","nonce":"<nonce>","purpose":"aci.report_data.v1"}
/// ```
///
/// with the `nonce` member the JSON literal `null` when absent. The nonce is
/// validated against the §4.2 charset so no accepted input ever needs JSON
/// escaping.
pub fn attestation_statement(
    keyset_digest: &str,
    nonce: Option<&str>,
) -> Result<Vec<u8>, InvalidNonce> {
    let nonce_member = match nonce {
        Some(nonce) if is_valid_nonce(nonce) => format!("\"{nonce}\""),
        Some(_) => return Err(InvalidNonce),
        None => "null".to_string(),
    };
    Ok(format!(
        "{{\"keyset_digest\":\"{keyset_digest}\",\"nonce\":{nonce_member},\"purpose\":\"{REPORT_DATA_PURPOSE}\"}}"
    )
    .into_bytes())
}

/// `report_data = sha256(statement bytes)` (§4.2).
pub fn report_data(statement: &[u8]) -> [u8; 32] {
    digest::sha256_raw(statement)
}

/// Place the 32-byte `report_data` in a 64-byte TEE report-data slot:
/// digest in bytes 0–31, zero in bytes 32–63 (§4.2).
pub fn report_data_slot(report_data: [u8; 32]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&report_data);
    out
}

/// A keyset frozen to its exact wire bytes. Sealed once at startup; the
/// service serves `bytes` (as `workload_keyset_b64`) and `digest` for its
/// whole lifetime, so the artifact stays byte-stable (§3).
#[derive(Debug, Clone)]
pub struct SealedWorkloadKeyset {
    keyset: WorkloadKeyset,
    bytes: Vec<u8>,
    digest: String,
}

impl SealedWorkloadKeyset {
    /// Serialize `keyset` once and freeze the bytes and digest.
    pub fn seal(keyset: WorkloadKeyset) -> Result<Self, serde_json::Error> {
        let bytes = serde_json::to_vec(&keyset)?;
        let digest = workload_keyset_digest(&bytes);
        Ok(Self {
            keyset,
            bytes,
            digest,
        })
    }

    /// Adopt served keyset bytes (verifier side): parse, keep the exact bytes,
    /// and recompute the digest from them.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, serde_json::Error> {
        let keyset: WorkloadKeyset = serde_json::from_slice(&bytes)?;
        let digest = workload_keyset_digest(&bytes);
        Ok(Self {
            keyset,
            bytes,
            digest,
        })
    }

    pub fn keyset(&self) -> &WorkloadKeyset {
        &self.keyset
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// `"sha256:" || hex` digest over [`Self::bytes`].
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The keyset as a JSON value (for evidence/debug surfaces, never for
    /// digest computation).
    pub fn to_value(&self) -> Value {
        serde_json::to_value(&self.keyset).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::types::{KeyedPublicKey, WorkloadKeyset};

    fn keyset() -> WorkloadKeyset {
        WorkloadKeyset {
            subject: Some("app-id:0x1234".to_string()),
            not_after: 1_790_000_000,
            receipt_signing_keys: vec![KeyedPublicKey {
                key_id: "r1".to_string(),
                algo: "ed25519".to_string(),
                public_key_hex: "aa".repeat(32),
            }],
            e2ee_public_keys: Vec::new(),
            tls_public_keys: Vec::new(),
        }
    }

    #[test]
    fn statement_bytes_match_the_spec_template_with_nonce() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let statement = attestation_statement(&digest, Some("nonce-123_A")).unwrap();
        assert_eq!(
            String::from_utf8(statement).unwrap(),
            format!(
                "{{\"keyset_digest\":\"{digest}\",\"nonce\":\"nonce-123_A\",\"purpose\":\"aci.report_data.v1\"}}"
            )
        );
    }

    #[test]
    fn statement_bytes_use_null_literal_without_nonce() {
        let digest = format!("sha256:{}", "ab".repeat(32));
        let statement = attestation_statement(&digest, None).unwrap();
        assert_eq!(
            String::from_utf8(statement).unwrap(),
            format!(
                "{{\"keyset_digest\":\"{digest}\",\"nonce\":null,\"purpose\":\"aci.report_data.v1\"}}"
            )
        );
    }

    #[test]
    fn nonce_charset_and_length_are_enforced() {
        assert!(is_valid_nonce("a"));
        assert!(is_valid_nonce(&"A0_-".repeat(32))); // exactly 128 chars
        assert!(!is_valid_nonce(""));
        assert!(!is_valid_nonce(&"a".repeat(129)));
        for bad in ["nonce with space", "nonce\"quote", "nonce+plus", "ноль"] {
            assert!(!is_valid_nonce(bad), "{bad:?} must be rejected");
            assert!(attestation_statement("sha256:00", Some(bad)).is_err());
        }
    }

    #[test]
    fn report_data_is_sha256_of_statement_bytes() {
        let statement = attestation_statement("sha256:00", None).unwrap();
        assert_eq!(report_data(&statement), digest::sha256_raw(&statement));
    }

    #[test]
    fn report_data_slot_zero_pads_to_64_bytes() {
        let rd = [0x42u8; 32];
        let slot = report_data_slot(rd);
        assert_eq!(&slot[..32], &rd);
        assert_eq!(&slot[32..], &[0u8; 32]);
    }

    #[test]
    fn sealed_keyset_digest_covers_the_exact_bytes() {
        let sealed = SealedWorkloadKeyset::seal(keyset()).unwrap();
        assert_eq!(sealed.digest(), digest::sha256_hex(sealed.bytes()));

        // Round-trip through served bytes reproduces the same digest.
        let adopted = SealedWorkloadKeyset::from_bytes(sealed.bytes().to_vec()).unwrap();
        assert_eq!(adopted.digest(), sealed.digest());
        assert_eq!(adopted.keyset(), sealed.keyset());
    }
}
