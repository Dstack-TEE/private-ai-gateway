//! Workload identity and attestation binding (ACI §3).
//!
//! The keyset is the unit of identity: its digest is over the keyset's JCS
//! form (§3.1, Appendix A), and the hardware quote binds the digest through
//! the fixed-byte attestation statement (§3.2). The report embeds the keyset
//! as a plain JSON object; a verifier canonicalizes the object it parsed.

use serde_json::Value;

use super::digest;
use super::types::WorkloadKeyset;
pub use aci_protocol::identity::*;

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
