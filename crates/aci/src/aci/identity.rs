//! Workload identity and attestation binding (ACI §3).
//!
//! The keyset is the unit of identity: its digest is over the keyset's JCS
//! form (§3.1, Appendix A), and the hardware quote binds the digest through
//! the fixed-byte attestation statement (§3.2). The report embeds the keyset
//! as a plain JSON object; a verifier canonicalizes the object it parsed.

use serde_json::Value;

use super::digest;
use super::types::WorkloadKeyset;

/// Purpose tag embedded in the attestation statement (§3.2).
pub const REPORT_DATA_PURPOSE: &str = "aci.report_data.v1";

/// Exact nonce length: a 32-byte value as lowercase hex (§3.2).
pub const NONCE_LEN: usize = 64;

#[derive(Debug, thiserror::Error)]
#[error("nonce must be exactly 64 lowercase hex characters (32 bytes)")]
pub struct InvalidNonce;

/// A statement input violates the escape-free template constraints (§3.2).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidStatementInput {
    #[error("keyset digest is not sha256: plus 64 lowercase hex")]
    KeysetDigest,
    #[error("nonce must be exactly 64 lowercase hex characters (32 bytes)")]
    Nonce,
}

/// `"sha256:" || hex(sha256(JCS(keyset)))` over the keyset object (§3.1).
pub fn workload_keyset_digest(keyset: &Value) -> Result<String, digest::JcsError> {
    Ok(digest::sha256_hex(&digest::jcs_bytes(keyset)?))
}

/// True when `nonce` is exactly 64 lowercase hex characters — a 32-byte
/// value (§3.2). Hex-only input keeps the statement template escape-free.
pub fn is_valid_nonce(nonce: &str) -> bool {
    nonce.len() == NONCE_LEN
        && nonce
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Build the exact statement bytes the TEE quote binds (§3.2):
///
/// ```text
/// {"keyset_digest":"sha256:<hex>","nonce":"<nonce>","purpose":"aci.report_data.v1"}
/// ```
///
/// with the `nonce` member the JSON literal `null` when absent. The nonce is
/// validated as 64 lowercase hex characters (§3.2), so no accepted input
/// ever needs JSON escaping.
pub fn attestation_statement(
    keyset_digest: &str,
    nonce: Option<&str>,
) -> Result<Vec<u8>, InvalidStatementInput> {
    // The template is escape-free only because both inputs are constrained
    // hex; a digest taken from a served report must not be able to inject
    // members into the statement.
    let hex = keyset_digest.strip_prefix("sha256:").unwrap_or("");
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(InvalidStatementInput::KeysetDigest);
    }
    let nonce_member = match nonce {
        Some(nonce) if is_valid_nonce(nonce) => format!("\"{nonce}\""),
        Some(_) => return Err(InvalidStatementInput::Nonce),
        None => "null".to_string(),
    };
    Ok(format!(
        "{{\"keyset_digest\":\"{keyset_digest}\",\"nonce\":{nonce_member},\"purpose\":\"{REPORT_DATA_PURPOSE}\"}}"
    )
    .into_bytes())
}

/// `report_data = sha256(statement bytes)` (§3.2).
pub fn report_data(statement: &[u8]) -> [u8; 32] {
    digest::sha256_raw(statement)
}

/// Place the 32-byte `report_data` in a 64-byte TEE report-data slot:
/// digest in bytes 0–31, zero in bytes 32–63 (§3.2).
pub fn report_data_slot(report_data: [u8; 32]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&report_data);
    out
}

/// A keyset frozen with its JCS form and digest. Sealed once at startup;
/// the report serves the keyset as a plain object (§4.1) and this digest
/// for the workload's lifetime.
#[derive(Debug, Clone)]
pub struct SealedWorkloadKeyset {
    keyset: WorkloadKeyset,
    value: Value,
    bytes: Vec<u8>,
    digest: String,
}

impl SealedWorkloadKeyset {
    /// Freeze `keyset` with its JCS bytes and digest.
    pub fn seal(keyset: WorkloadKeyset) -> Result<Self, serde_json::Error> {
        let value = serde_json::to_value(&keyset)?;
        let bytes = digest::jcs_bytes(&value).map_err(serde::ser::Error::custom)?;
        let digest = digest::sha256_hex(&bytes);
        Ok(Self {
            keyset,
            value,
            bytes,
            digest,
        })
    }

    /// Adopt a served keyset object (verifier side): canonicalize exactly the
    /// value that was parsed — unknown members included — and recompute the
    /// digest from it.
    pub fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        let keyset: WorkloadKeyset = serde_json::from_value(value.clone())?;
        let bytes = digest::jcs_bytes(&value).map_err(serde::ser::Error::custom)?;
        let digest = digest::sha256_hex(&bytes);
        Ok(Self {
            keyset,
            value,
            bytes,
            digest,
        })
    }

    pub fn keyset(&self) -> &WorkloadKeyset {
        &self.keyset
    }

    /// The keyset's JCS bytes — the digest input (§3.1).
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// `"sha256:" || hex` digest over [`Self::bytes`].
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// The keyset object as the report serves it (§4.1).
    pub fn to_value(&self) -> Value {
        self.value.clone()
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
        let nonce = "1f".repeat(32);
        let statement = attestation_statement(&digest, Some(&nonce)).unwrap();
        assert_eq!(
            String::from_utf8(statement).unwrap(),
            format!(
                "{{\"keyset_digest\":\"{digest}\",\"nonce\":\"{nonce}\",\"purpose\":\"aci.report_data.v1\"}}"
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
    fn nonce_must_be_exactly_64_lowercase_hex_chars() {
        assert!(is_valid_nonce(&"0a".repeat(32)));
        assert!(!is_valid_nonce(""));
        assert!(!is_valid_nonce(&"a".repeat(63)));
        assert!(!is_valid_nonce(&"a".repeat(65)));
        let uppercase = format!("A{}", "a".repeat(63));
        let nonhex = format!("g{}", "a".repeat(63));
        let quote = format!("\"{}", "a".repeat(63));
        for bad in [uppercase.as_str(), nonhex.as_str(), quote.as_str()] {
            assert!(!is_valid_nonce(bad), "{bad:?} must be rejected");
            assert!(attestation_statement("sha256:00", Some(bad)).is_err());
        }
    }

    #[test]
    fn report_data_slot_zero_pads_to_64_bytes() {
        let rd = [0x42u8; 32];
        let slot = report_data_slot(rd);
        assert_eq!(&slot[..32], &rd);
        assert_eq!(&slot[32..], &[0u8; 32]);
    }

    #[test]
    fn adopting_a_served_keyset_reproduces_the_sealed_digest() {
        let sealed = SealedWorkloadKeyset::seal(keyset()).unwrap();
        // The verifier path (`from_value`) must agree with the producer path
        // (`seal`) on the same keyset.
        let adopted = SealedWorkloadKeyset::from_value(sealed.to_value()).unwrap();
        assert_eq!(adopted.digest(), sealed.digest());
        assert_eq!(adopted.keyset(), sealed.keyset());
    }

    #[test]
    fn served_object_with_unknown_member_changes_the_digest() {
        let sealed = SealedWorkloadKeyset::seal(keyset()).unwrap();
        let mut value = sealed.to_value();
        value["extra"] = Value::String("x".to_string());
        // Unknown members are canonicalized too: WorkloadKeyset parsing may
        // reject or ignore them, but the digest must not silently drop them.
        if let Ok(adopted) = SealedWorkloadKeyset::from_value(value) {
            assert_ne!(adopted.digest(), sealed.digest());
        }
    }
}
