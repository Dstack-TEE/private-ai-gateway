//! Shape checks for the proxy's offline-verifiable version 1 bundle.

use base64::{
    engine::general_purpose::{STANDARD as BASE64, URL_SAFE_NO_PAD},
    Engine as _,
};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use time::{format_description::well_known::Rfc3339, OffsetDateTime, UtcOffset};

use crate::aci::digest::sha256_bare_hex;

pub(super) const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Evidence {
    version: u32,
    secret_id: String,
    manifest_sha256: String,
    manifest: String,
    coordinator_attestation: CoordinatorAttestation,
    mesh_ca_pem: String,
    secret_service_cert: String,
    request_public_key: String,
    encapsulated_key: String,
    signature: String,
    exchanged_at: String,
    expires_at: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CoordinatorAttestation {
    nonce: String,
    document: String,
    attested_at: String,
}

fn invalid(field: &str) -> String {
    format!("invalid Privatemode active-secret evidence: {field}")
}

fn bytes(value: &str, maximum: usize, field: &str) -> Result<Vec<u8>, String> {
    if value.is_empty() || value.len() > maximum.div_ceil(3) * 4 {
        return Err(invalid(field));
    }
    let decoded = BASE64.decode(value).map_err(|_| invalid(field))?;
    if decoded.is_empty() || decoded.len() > maximum || BASE64.encode(&decoded) != value {
        return Err(invalid(field));
    }
    Ok(decoded)
}

pub(super) fn url_id(secret_id: &str) -> Result<String, String> {
    let id = bytes(secret_id, 32, "secret_id")?;
    if id.len() != 32 {
        return Err(invalid("secret_id must encode 32 bytes"));
    }
    Ok(URL_SAFE_NO_PAD.encode(id))
}

fn timestamp(value: &str, field: &str) -> Result<OffsetDateTime, String> {
    let parsed = OffsetDateTime::parse(value, &Rfc3339).map_err(|_| invalid(field))?;
    if value.len() > 64 || parsed.offset() != UtcOffset::UTC {
        return Err(invalid(field));
    }
    Ok(parsed)
}

/// Validate encoding and internal identifiers, not Contrast or TEE signatures.
/// Keep the original JSON bytes for the relying party's independent verifier.
pub(super) fn validate(body: &[u8], secret_id: &str, requested_id: &str) -> Result<Value, String> {
    let evidence: Evidence = serde_json::from_slice(body).map_err(|_| invalid("JSON shape"))?;
    if evidence.version != 1 {
        return Err(invalid("version must be 1"));
    }
    if evidence.secret_id != secret_id || url_id(&evidence.secret_id)? != requested_id {
        return Err(invalid(
            "secret_id does not match response and evidence URL",
        ));
    }
    let public_key = bytes(&evidence.request_public_key, 4096, "request_public_key")?;
    let public_key_id = Sha256::digest(&public_key);
    if BASE64.encode(public_key_id) != secret_id {
        return Err(invalid("secret_id does not match request_public_key"));
    }
    let manifest = bytes(&evidence.manifest, 4 * 1024 * 1024, "manifest")?;
    if evidence.manifest_sha256.len() != 64
        || !evidence
            .manifest_sha256
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || sha256_bare_hex(&manifest) != evidence.manifest_sha256
    {
        return Err(invalid("manifest_sha256"));
    }
    if !serde_json::from_slice::<Value>(&manifest).is_ok_and(|v| v.is_object()) {
        return Err(invalid("manifest must be a JSON object"));
    }
    bytes(
        &evidence.coordinator_attestation.nonce,
        1024,
        "coordinator_attestation.nonce",
    )?;
    bytes(
        &evidence.coordinator_attestation.document,
        1024 * 1024,
        "coordinator_attestation.document",
    )?;
    bytes(
        &evidence.secret_service_cert,
        64 * 1024,
        "secret_service_cert",
    )?;
    bytes(&evidence.encapsulated_key, 4096, "encapsulated_key")?;
    bytes(&evidence.signature, 1024, "signature")?;
    if evidence.mesh_ca_pem.len() > 64 * 1024
        || !evidence
            .mesh_ca_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
        || !evidence
            .mesh_ca_pem
            .trim_end()
            .ends_with("-----END CERTIFICATE-----")
    {
        return Err(invalid("mesh_ca_pem"));
    }
    let certs = rustls_pemfile::certs(&mut evidence.mesh_ca_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid("mesh_ca_pem"))?;
    if certs.is_empty() || certs.len() > 16 || certs.iter().any(|c| c.is_empty()) {
        return Err(invalid("mesh_ca_pem"));
    }
    let attested = timestamp(&evidence.coordinator_attestation.attested_at, "attested_at")?;
    let exchanged = timestamp(&evidence.exchanged_at, "exchanged_at")?;
    let expires = timestamp(&evidence.expires_at, "expires_at")?;
    if attested > exchanged || exchanged >= expires {
        return Err(invalid("timestamps out of order"));
    }
    Ok(serde_json::json!({
        "type": "privatemode_active_secret_evidence",
        "digest": format!("sha256:{}", sha256_bare_hex(body)),
        "data": format!("data:application/json;base64,{}", BASE64.encode(body)),
        "secret_id": evidence.secret_id,
        "manifest_sha256": evidence.manifest_sha256,
    }))
}
