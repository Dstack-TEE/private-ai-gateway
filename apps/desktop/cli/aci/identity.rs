//! Workload identity and attestation binding (ACI §3).
//!
//! The keyset is the unit of identity: its digest is over the keyset's JCS
//! form (§3.1, Appendix A), and the hardware quote binds the digest through
//! the fixed-byte attestation statement (§3.2). The report embeds the keyset
//! as a plain JSON object; a verifier canonicalizes the object it parsed.

use serde_json::Value;

use super::digest;

/// Purpose tag embedded in the attestation statement (§3.2).
const REPORT_DATA_PURPOSE: &str = "aci.report_data.v1";

/// Exact nonce length: a 32-byte value as lowercase hex (§3.2).
const NONCE_LEN: usize = 64;

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
fn is_valid_nonce(nonce: &str) -> bool {
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
pub(crate) fn report_data_slot(report_data: [u8; 32]) -> [u8; 64] {
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(&report_data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
