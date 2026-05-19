//! Identity, keyset, and attestation digest computations from ACI §4.
//!
//! Pure functions: they take typed protocol structures from
//! [`super::types`] and return digests, signing payloads, and the
//! 32-byte `report_data` value the TEE quote must cover. No I/O,
//! no key state, no framework dependency.

use super::canonical::{self, CanonicalError};
use super::types::{
    AttestationStatement, KeysetEndorsementPayload, PublicKeyMaterial, WorkloadIdentity,
    WorkloadKeyset,
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
