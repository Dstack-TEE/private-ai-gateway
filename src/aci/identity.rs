//! Identity, keyset, and attestation digest computations from ACI §4.
//!
//! Pure functions: they take typed protocol structures from
//! [`super::types`] and return digests, signing payloads, and the
//! 32-byte `report_data` value the TEE quote must cover. No I/O,
//! no key state, no framework dependency.

use super::canonical::{self, CanonicalError};
use super::types::{
    AttestationStatement, KeysetEndorsementPayload, KeysetRevocationPayload, PublicKeyMaterial,
    WorkloadIdentity, WorkloadKeyset,
};

/// Return `"sha256:" || hex(sha256(JCS(identity.public_key)))`.
///
/// `workload_id` covers only the identity public key. Subject changes
/// rotate the keyset, not the stable identity.
pub fn workload_id_for_key(pk: &PublicKeyMaterial) -> Result<String, CanonicalError> {
    canonical::jcs_sha256_hex(&pk.to_canonical_value())
}

/// Convenience: read the public key out of `identity` and hash it.
pub fn workload_id(identity: &WorkloadIdentity) -> Result<String, CanonicalError> {
    workload_id_for_key(&identity.public_key)
}

/// Return `"sha256:" || hex(sha256(JCS(workload_keyset)))`.
pub fn workload_keyset_digest(keyset: &WorkloadKeyset) -> Result<String, CanonicalError> {
    canonical::jcs_sha256_hex(&keyset.to_canonical_value())
}

/// Build the named statement that `report_data` covers.
///
/// The caller is responsible for supplying `nonce` exactly as
/// received: the URL-decoded UTF-8 value of the `nonce` query
/// parameter, or `None` if the parameter was omitted.
pub fn attestation_statement(
    keyset: &WorkloadKeyset,
    nonce: Option<String>,
) -> Result<AttestationStatement, CanonicalError> {
    Ok(AttestationStatement {
        workload_id: workload_id(&keyset.workload_identity)?,
        workload_keyset_digest: workload_keyset_digest(keyset)?,
        nonce,
    })
}

/// `report_data = sha256(JCS(attestation_statement))`.
///
/// Returns the raw 32 bytes a verifier profile will pad, place, or
/// lift into TDX / SEV-SNP report-data slots.
pub fn report_data(statement: &AttestationStatement) -> Result<[u8; 32], CanonicalError> {
    canonical::jcs_sha256_raw(&statement.to_canonical_value())
}

/// Canonical bytes the identity key signs for `keyset_endorsement`.
///
/// ACI §4.2 names this `keyset_endorsement_payload` and requires the
/// signature to be over the JCS of the named object, not over the raw
/// keyset digest. Callers MUST sign exactly these bytes.
pub fn keyset_endorsement_payload(keyset: &WorkloadKeyset) -> Result<Vec<u8>, CanonicalError> {
    let payload = KeysetEndorsementPayload {
        workload_keyset_digest: workload_keyset_digest(keyset)?,
    };
    canonical::canonicalize(&payload.to_canonical_value())
}

/// Canonical bytes the identity key signs to revoke a keyset digest (§4.7).
///
/// Mirrors [`keyset_endorsement_payload`]: the same JCS object shape under the
/// `aci.keyset.revocation.v1` purpose tag. The caller supplies the digest of
/// the keyset being repudiated and MUST sign exactly these bytes.
pub fn keyset_revocation_payload(workload_keyset_digest: &str) -> Result<Vec<u8>, CanonicalError> {
    let payload = KeysetRevocationPayload {
        workload_keyset_digest: workload_keyset_digest.to_string(),
    };
    canonical::canonicalize(&payload.to_canonical_value())
}

/// Fingerprint over the keyset's key material — identity, receipt, E2EE, and
/// TLS keys plus the subject — excluding the rotating `keyset_epoch`.
///
/// Two keysets with the same fingerprint carry identical key material; only the
/// epoch may differ. The launcher uses this to decide, on restart, whether it
/// is serving the same keys (keep the epoch version) or new ones (bump it),
/// without the ever-changing `not_after` counting as a key change.
pub fn keyset_material_fingerprint(keyset: &WorkloadKeyset) -> Result<String, CanonicalError> {
    let mut value = keyset.to_canonical_value();
    if let Some(obj) = value.as_object_mut() {
        obj.remove("keyset_epoch");
    }
    canonical::jcs_sha256_hex(&value)
}
