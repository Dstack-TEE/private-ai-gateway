//! ACI E2EE v3 sealing (§7.1) and the inherited dstack-vllm-proxy legacy
//! E2EE modes (§13).
//!
//! One construction seals everything in v3, parameterized by a context
//! string and a recipient X25519 public key:
//!
//! ```text
//! context           = "aci.e2ee.v3.request" | "aci.e2ee.v3.response"
//! shared_secret     = X25519(ephemeral_private_key, recipient_public_key)
//! key               = HKDF-SHA256(salt = <absent>, ikm = shared_secret,
//!                                 info = UTF-8(context), length = 32)
//! request_aad       = UTF-8(context) || 0x00 || UTF-8(model) || 0x00 || UTF-8(client_key_hex)
//! response_aad      = UTF-8(context) || 0x00 || UTF-8(model)
//! sealed            = ephemeral_public_key (32) || gcm_nonce (12) || ciphertext || tag (16)
//! ```
//!
//! The legacy `X-Signing-Algo` profile (`ecdsa` / `ed25519` labels) keeps its
//! own HKDF context strings, hex-encoded per-field payloads, and no AAD.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::Aes256Gcm;
use curve25519_dalek::edwards::CompressedEdwardsY;
use ed25519_dalek::SigningKey as Ed25519SigningKey;
use hkdf::Hkdf;
use k256::ecdh::diffie_hellman;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::{EncodedPoint, PublicKey, SecretKey};
use rand::rngs::OsRng;
use sha2::{Digest, Sha256, Sha512};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519SecretKey};

use super::keys::KeyError;

/// The ACI E2EE scheme version this document defines (§7). `1` and `2` are
/// reserved-historical; `1` survives only as the §13 legacy label.
pub const E2EE_VERSION_V3: &str = "3";
pub const E2EE_VERSION_V1: &str = "1";

/// The one ACI v1 E2EE algorithm (§7.1, Appendix A).
pub const E2EE_ALGO_X25519_AESGCM: &str = "x25519-aes-256-gcm-hkdf-sha256";
/// §13 legacy `X-Signing-Algo` labels.
pub const E2EE_ALGO_LEGACY_ECDSA: &str = "ecdsa";
pub const E2EE_ALGO_LEGACY_ED25519: &str = "ed25519";

/// §7.1 context strings; also the HKDF `info` values.
pub const E2EE_CONTEXT_REQUEST: &str = "aci.e2ee.v3.request";
pub const E2EE_CONTEXT_RESPONSE: &str = "aci.e2ee.v3.response";

const LEGACY_ECDSA_HKDF_INFO: &[u8] = b"ecdsa_encryption";
const LEGACY_ED25519_HKDF_INFO: &[u8] = b"ed25519_encryption";
const SECP256K1_PUBLIC_KEY_LEN: usize = 65;
const X25519_PUBLIC_KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

// ---------- ACI E2EE v3 (§7.1) ----------

/// The §7.1 AAD. The request appends `0x00 || client_key_hex` so a replay
/// cannot reseal to another `X-Client-Pub-Key`; the response omits it (`None`).
pub fn e2ee_aad(context: &str, model: &str, client_public_key_hex: Option<&str>) -> Vec<u8> {
    let mut aad = context.as_bytes().to_vec();
    aad.push(0x00);
    aad.extend_from_slice(model.as_bytes());
    if let Some(client) = client_public_key_hex {
        aad.push(0x00);
        aad.extend_from_slice(client.as_bytes());
    }
    aad
}

/// Seal one unit to `recipient_public_key` with a fresh ephemeral key and
/// nonce, returning the raw §7.1 sealed bytes (callers base64 them).
/// `client_public_key_hex` is the request's `X-Client-Pub-Key`, `None` for a response.
pub fn seal_v3(
    recipient_public_key: &[u8; 32],
    context: &str,
    model: &str,
    client_public_key_hex: Option<&str>,
    plaintext: &[u8],
) -> Result<Vec<u8>, KeyError> {
    let recipient = X25519PublicKey::from(*recipient_public_key);
    let ephemeral = X25519SecretKey::from(rand::random::<[u8; 32]>());
    let ephemeral_public = X25519PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&recipient);
    let cipher = v3_cipher(shared.as_bytes(), context)?;
    let nonce_bytes: [u8; NONCE_LEN] = rand::random();
    let ciphertext = cipher
        .encrypt(
            &nonce_bytes.into(),
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad: &e2ee_aad(context, model, client_public_key_hex),
            },
        )
        .map_err(|e| KeyError::Crypto(format!("E2EE v3 seal failed: {e}")))?;

    let mut out = Vec::with_capacity(X25519_PUBLIC_KEY_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(ephemeral_public.as_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Unseal one §7.1 sealed unit with the recipient's X25519 secret.
/// `client_public_key_hex` reproduces the request AAD's `X-Client-Pub-Key`, `None` for a response.
pub fn unseal_v3(
    recipient_secret: &X25519SecretKey,
    context: &str,
    model: &str,
    client_public_key_hex: Option<&str>,
    sealed: &[u8],
) -> Result<Vec<u8>, KeyError> {
    if sealed.len() < X25519_PUBLIC_KEY_LEN + NONCE_LEN + TAG_LEN {
        return Err(KeyError::Crypto(format!(
            "E2EE v3 sealed unit too short: got {} bytes",
            sealed.len()
        )));
    }
    let eph_bytes: [u8; X25519_PUBLIC_KEY_LEN] = sealed[..X25519_PUBLIC_KEY_LEN]
        .try_into()
        .expect("ephemeral public key length is checked");
    let eph = X25519PublicKey::from(eph_bytes);
    let nonce_bytes: [u8; NONCE_LEN] = sealed
        [X25519_PUBLIC_KEY_LEN..X25519_PUBLIC_KEY_LEN + NONCE_LEN]
        .try_into()
        .expect("nonce length is checked");
    let ciphertext = &sealed[X25519_PUBLIC_KEY_LEN + NONCE_LEN..];
    let shared = recipient_secret.diffie_hellman(&eph);
    let cipher = v3_cipher(shared.as_bytes(), context)?;
    cipher
        .decrypt(
            &nonce_bytes.into(),
            aes_gcm::aead::Payload {
                msg: ciphertext,
                aad: &e2ee_aad(context, model, client_public_key_hex),
            },
        )
        .map_err(|e| KeyError::Crypto(format!("E2EE v3 unseal failed: {e}")))
}

fn v3_cipher(shared: &[u8], context: &str) -> Result<Aes256Gcm, KeyError> {
    let hk = Hkdf::<Sha256>::new(None, shared);
    let mut key = [0u8; 32];
    hk.expand(context.as_bytes(), &mut key)
        .map_err(|e| KeyError::Crypto(format!("HKDF expand failed: {e}")))?;
    Ok(Aes256Gcm::new_from_slice(&key).expect("AES-256 key length is fixed"))
}

/// Parse a §7.1 public key: 32 hex-encoded bytes, no `0x` prefix (§3).
pub fn x25519_public_key_from_hex(value: &str) -> Result<[u8; 32], KeyError> {
    let bytes = hex::decode(value)
        .map_err(|e| KeyError::Crypto(format!("invalid X25519 public key hex: {e}")))?;
    bytes.as_slice().try_into().map_err(|_| {
        KeyError::Crypto(format!(
            "X25519 public key must be 32 bytes, got {}",
            bytes.len()
        ))
    })
}

/// Canonical lowercase hex for a §7.1 public key.
pub fn normalize_x25519_public_key_hex(value: &str) -> Result<String, KeyError> {
    Ok(hex::encode(x25519_public_key_from_hex(value)?))
}

pub fn x25519_public_key_hex(secret: &X25519SecretKey) -> String {
    hex::encode(X25519PublicKey::from(secret).as_bytes())
}

pub fn x25519_secret_key_from_bytes(bytes: &[u8]) -> Result<X25519SecretKey, KeyError> {
    let bytes: [u8; 32] = bytes.try_into().map_err(|_| {
        KeyError::Crypto(format!(
            "invalid X25519 E2EE key: must be 32 bytes, got {}",
            bytes.len()
        ))
    })?;
    Ok(X25519SecretKey::from(bytes))
}

// ---------- §13 legacy X-Signing-Algo modes ----------

pub fn normalize_secp256k1_public_key_hex(value: &str) -> Result<String, KeyError> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .map_err(|e| KeyError::Crypto(format!("invalid secp256k1 public key hex: {e}")))?;
    let encoded = match bytes.as_slice() {
        [0x04, rest @ ..] if rest.len() == 64 => EncodedPoint::from_bytes([&[0x04], rest].concat()),
        rest if rest.len() == 64 => EncodedPoint::from_bytes([&[0x04], rest].concat()),
        _ => {
            return Err(KeyError::Crypto(format!(
                "secp256k1 public key must be 64 or 65 bytes, got {}",
                bytes.len()
            )));
        }
    }
    .map_err(|e| KeyError::Crypto(format!("invalid secp256k1 public key: {e}")))?;
    PublicKey::from_sec1_bytes(encoded.as_bytes())
        .map_err(|e| KeyError::Crypto(format!("invalid secp256k1 public key: {e}")))?;
    Ok(hex::encode(encoded.as_bytes()))
}

pub fn public_key_from_secret(secret: &SecretKey) -> String {
    hex::encode(secret.public_key().to_encoded_point(false).as_bytes())
}

pub fn legacy_ecdsa_public_key_from_secret(secret: &SecretKey) -> String {
    let public_key = secret.public_key().to_encoded_point(false);
    hex::encode(&public_key.as_bytes()[1..])
}

pub fn ed25519_public_key_hex(secret: &Ed25519SigningKey) -> String {
    hex::encode(secret.verifying_key().as_bytes())
}

pub fn encrypt_legacy_for_public_key(
    signing_algo: &str,
    recipient_public_key_hex: &str,
    plaintext: &[u8],
    aad: Option<&[u8]>,
) -> Result<String, KeyError> {
    match signing_algo {
        E2EE_ALGO_LEGACY_ECDSA => {
            let recipient = public_key_from_hex(recipient_public_key_hex)?;
            let ephemeral = SecretKey::random(&mut OsRng);
            let shared = diffie_hellman(ephemeral.to_nonzero_scalar(), recipient.as_affine());
            let cipher = legacy_cipher_from_shared_secret(
                shared.raw_secret_bytes().as_ref(),
                E2EE_ALGO_LEGACY_ECDSA,
            )?;
            let nonce_bytes: [u8; NONCE_LEN] = rand::random();
            let ciphertext = cipher
                .encrypt(
                    &nonce_bytes.into(),
                    aes_gcm::aead::Payload {
                        msg: plaintext,
                        aad: aad.unwrap_or(&[]),
                    },
                )
                .map_err(|e| KeyError::Crypto(format!("legacy E2EE encrypt failed: {e}")))?;

            let mut out =
                Vec::with_capacity(SECP256K1_PUBLIC_KEY_LEN + NONCE_LEN + ciphertext.len());
            out.extend_from_slice(ephemeral.public_key().to_encoded_point(false).as_bytes());
            out.extend_from_slice(&nonce_bytes);
            out.extend_from_slice(&ciphertext);
            Ok(hex::encode(out))
        }
        E2EE_ALGO_LEGACY_ED25519 => {
            let recipient = ed25519_public_to_x25519_public_key(recipient_public_key_hex)?;
            let secret = X25519SecretKey::from(rand::random::<[u8; 32]>());
            let public = X25519PublicKey::from(&secret);
            let shared = secret.diffie_hellman(&recipient);
            let cipher =
                legacy_cipher_from_shared_secret(shared.as_bytes(), E2EE_ALGO_LEGACY_ED25519)?;
            let nonce_bytes: [u8; NONCE_LEN] = rand::random();
            let ciphertext = cipher
                .encrypt(
                    &nonce_bytes.into(),
                    aes_gcm::aead::Payload {
                        msg: plaintext,
                        aad: aad.unwrap_or(&[]),
                    },
                )
                .map_err(|e| KeyError::Crypto(format!("legacy E2EE encrypt failed: {e}")))?;

            let mut out = Vec::with_capacity(X25519_PUBLIC_KEY_LEN + NONCE_LEN + ciphertext.len());
            out.extend_from_slice(public.as_bytes());
            out.extend_from_slice(&nonce_bytes);
            out.extend_from_slice(&ciphertext);
            Ok(hex::encode(out))
        }
        other => Err(KeyError::UnsupportedAlgo(other.to_string())),
    }
}

pub fn decrypt_legacy_ecdsa_with_secret_key(
    recipient_secret: &SecretKey,
    ciphertext_hex: &str,
    aad: Option<&[u8]>,
) -> Result<Vec<u8>, KeyError> {
    let blob = hex::decode(ciphertext_hex.strip_prefix("0x").unwrap_or(ciphertext_hex))
        .map_err(|e| KeyError::Crypto(format!("invalid legacy E2EE ciphertext hex: {e}")))?;
    if blob.len() < SECP256K1_PUBLIC_KEY_LEN + NONCE_LEN + TAG_LEN {
        return Err(KeyError::Crypto(format!(
            "legacy ECDSA E2EE ciphertext too short: got {} bytes",
            blob.len()
        )));
    }
    let eph = PublicKey::from_sec1_bytes(&blob[..SECP256K1_PUBLIC_KEY_LEN])
        .map_err(|e| KeyError::Crypto(format!("invalid legacy ECDSA ephemeral public key: {e}")))?;
    let nonce_bytes: [u8; NONCE_LEN] = blob
        [SECP256K1_PUBLIC_KEY_LEN..SECP256K1_PUBLIC_KEY_LEN + NONCE_LEN]
        .try_into()
        .expect("nonce length is checked");
    let ciphertext = &blob[SECP256K1_PUBLIC_KEY_LEN + NONCE_LEN..];
    let shared = diffie_hellman(recipient_secret.to_nonzero_scalar(), eph.as_affine());
    let cipher = legacy_cipher_from_shared_secret(
        shared.raw_secret_bytes().as_ref(),
        E2EE_ALGO_LEGACY_ECDSA,
    )?;
    cipher
        .decrypt(
            &nonce_bytes.into(),
            aes_gcm::aead::Payload {
                msg: ciphertext,
                aad: aad.unwrap_or(&[]),
            },
        )
        .map_err(|e| KeyError::Crypto(format!("legacy ECDSA E2EE decrypt failed: {e}")))
}

pub fn decrypt_legacy_ed25519_with_secret_key(
    recipient_secret: &Ed25519SigningKey,
    ciphertext_hex: &str,
    aad: Option<&[u8]>,
) -> Result<Vec<u8>, KeyError> {
    let blob = hex::decode(ciphertext_hex.strip_prefix("0x").unwrap_or(ciphertext_hex))
        .map_err(|e| KeyError::Crypto(format!("invalid legacy E2EE ciphertext hex: {e}")))?;
    if blob.len() < X25519_PUBLIC_KEY_LEN + NONCE_LEN + TAG_LEN {
        return Err(KeyError::Crypto(format!(
            "legacy Ed25519 E2EE ciphertext too short: got {} bytes",
            blob.len()
        )));
    }
    let eph_bytes: [u8; X25519_PUBLIC_KEY_LEN] = blob[..X25519_PUBLIC_KEY_LEN]
        .try_into()
        .expect("ephemeral public key length is checked");
    let eph = X25519PublicKey::from(eph_bytes);
    let nonce_bytes: [u8; NONCE_LEN] = blob
        [X25519_PUBLIC_KEY_LEN..X25519_PUBLIC_KEY_LEN + NONCE_LEN]
        .try_into()
        .expect("nonce length is checked");
    let ciphertext = &blob[X25519_PUBLIC_KEY_LEN + NONCE_LEN..];
    let secret = ed25519_private_to_x25519_private_key(recipient_secret);
    let shared = secret.diffie_hellman(&eph);
    let cipher = legacy_cipher_from_shared_secret(shared.as_bytes(), E2EE_ALGO_LEGACY_ED25519)?;
    cipher
        .decrypt(
            &nonce_bytes.into(),
            aes_gcm::aead::Payload {
                msg: ciphertext,
                aad: aad.unwrap_or(&[]),
            },
        )
        .map_err(|e| KeyError::Crypto(format!("legacy Ed25519 E2EE decrypt failed: {e}")))
}

pub fn secret_key_from_bytes(bytes: &[u8]) -> Result<SecretKey, KeyError> {
    SecretKey::from_slice(bytes)
        .map_err(|e| KeyError::Crypto(format!("invalid secp256k1 E2EE key: {e}")))
}

fn public_key_from_hex(value: &str) -> Result<PublicKey, KeyError> {
    let normalized = normalize_secp256k1_public_key_hex(value)?;
    let bytes = hex::decode(normalized)
        .map_err(|e| KeyError::Crypto(format!("invalid secp256k1 public key hex: {e}")))?;
    PublicKey::from_sec1_bytes(&bytes)
        .map_err(|e| KeyError::Crypto(format!("invalid secp256k1 public key: {e}")))
}

fn legacy_cipher_from_shared_secret(
    shared: &[u8],
    signing_algo: &str,
) -> Result<Aes256Gcm, KeyError> {
    let info = match signing_algo {
        E2EE_ALGO_LEGACY_ECDSA => LEGACY_ECDSA_HKDF_INFO,
        E2EE_ALGO_LEGACY_ED25519 => LEGACY_ED25519_HKDF_INFO,
        other => return Err(KeyError::UnsupportedAlgo(other.to_string())),
    };
    let hk = Hkdf::<Sha256>::new(None, shared);
    let mut key = [0u8; 32];
    hk.expand(info, &mut key)
        .map_err(|e| KeyError::Crypto(format!("legacy HKDF expand failed: {e}")))?;
    Ok(Aes256Gcm::new_from_slice(&key).expect("AES-256 key length is fixed"))
}

fn ed25519_public_to_x25519_public_key(value: &str) -> Result<X25519PublicKey, KeyError> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .map_err(|e| KeyError::Crypto(format!("invalid Ed25519 public key hex: {e}")))?;
    let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        KeyError::Crypto(format!(
            "Ed25519 public key must be 32 bytes, got {}",
            bytes.len()
        ))
    })?;
    let point = CompressedEdwardsY(bytes)
        .decompress()
        .ok_or_else(|| KeyError::Crypto("invalid Ed25519 public key point".to_string()))?;
    Ok(X25519PublicKey::from(point.to_montgomery().to_bytes()))
}

fn ed25519_private_to_x25519_private_key(secret: &Ed25519SigningKey) -> X25519SecretKey {
    let digest = Sha512::digest(secret.to_bytes());
    let mut scalar = [0u8; 32];
    scalar.copy_from_slice(&digest[..32]);
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    X25519SecretKey::from(scalar)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipient() -> (X25519SecretKey, [u8; 32]) {
        let secret = x25519_secret_key_from_bytes(&[0x37u8; 32]).unwrap();
        let public = *X25519PublicKey::from(&secret).as_bytes();
        (secret, public)
    }

    #[test]
    fn aad_binds_context_model_and_the_request_client_key() {
        let (req, resp) = (E2EE_CONTEXT_REQUEST, E2EE_CONTEXT_RESPONSE);
        // Response AAD: context || 0x00 || model. Request appends 0x00 || client key.
        assert_eq!(e2ee_aad(resp, "m", None), b"aci.e2ee.v3.response\x00m");
        assert_eq!(
            e2ee_aad(req, "m", Some("cc")),
            b"aci.e2ee.v3.request\x00m\x00cc"
        );
    }

    #[test]
    fn v3_round_trip_recovers_plaintext_and_layout() {
        let (secret, public) = recipient();
        let req = E2EE_CONTEXT_REQUEST;
        let sealed = seal_v3(&public, req, "m", Some("aa"), b"hello v3").unwrap();
        // ephemeral pub (32) || nonce (12) || ct (8) || tag (16)
        assert_eq!(sealed.len(), 32 + 12 + 8 + 16);
        let opened = unseal_v3(&secret, req, "m", Some("aa"), &sealed).unwrap();
        assert_eq!(opened, b"hello v3");
    }

    #[test]
    fn v3_binds_model_context_and_request_client_key_into_the_aad() {
        let (secret, public) = recipient();
        let (req, resp) = (E2EE_CONTEXT_REQUEST, E2EE_CONTEXT_RESPONSE);
        let sealed = seal_v3(&public, req, "model-a", Some("aa"), b"bound").unwrap();
        // A wrong model, context, or X-Client-Pub-Key fails auth; the swapped-key
        // case (Some("bb")) is the replay-reseal the fix closes (§7.2).
        assert!(unseal_v3(&secret, req, "model-b", Some("aa"), &sealed).is_err());
        assert!(unseal_v3(&secret, resp, "model-a", Some("aa"), &sealed).is_err());
        assert!(unseal_v3(&secret, req, "model-a", Some("bb"), &sealed).is_err());
        assert!(unseal_v3(&secret, req, "model-a", Some("aa"), &sealed).is_ok());
    }

    #[test]
    fn v3_uses_fresh_ephemeral_keys_per_unit() {
        let (_, public) = recipient();
        let a = seal_v3(&public, E2EE_CONTEXT_REQUEST, "m", None, b"x").unwrap();
        let b = seal_v3(&public, E2EE_CONTEXT_REQUEST, "m", None, b"x").unwrap();
        assert_ne!(&a[..32], &b[..32], "ephemeral public keys must differ");
    }

    #[test]
    fn v3_rejects_truncated_sealed_units() {
        let (secret, public) = recipient();
        let sealed = seal_v3(&public, E2EE_CONTEXT_REQUEST, "m", None, b"x").unwrap();
        assert!(unseal_v3(&secret, E2EE_CONTEXT_REQUEST, "m", None, &sealed[..59]).is_err());
    }

    #[test]
    fn x25519_public_key_rejects_0x_prefix_and_wrong_length() {
        // §3: public keys are hex with no 0x prefix; §7.4 rejects anything
        // that does not parse as 32 hex-encoded bytes.
        let (_, public) = recipient();
        let hexstr = hex::encode(public);
        assert_eq!(normalize_x25519_public_key_hex(&hexstr).unwrap(), hexstr);
        assert!(x25519_public_key_from_hex(&format!("0x{hexstr}")).is_err());
        assert!(x25519_public_key_from_hex("00ff").is_err());
    }

    #[test]
    fn legacy_ecdsa_round_trip_still_works() {
        let secret = secret_key_from_bytes(&[0x21u8; 32]).unwrap();
        let public_hex = public_key_from_secret(&secret);
        let ciphertext =
            encrypt_legacy_for_public_key(E2EE_ALGO_LEGACY_ECDSA, &public_hex, b"legacy", None)
                .unwrap();
        assert_eq!(
            decrypt_legacy_ecdsa_with_secret_key(&secret, &ciphertext, None).unwrap(),
            b"legacy"
        );
    }

    #[test]
    fn legacy_ed25519_round_trip_still_works() {
        let secret = Ed25519SigningKey::from_bytes(&[0x42u8; 32]);
        let public_hex = ed25519_public_key_hex(&secret);
        let ciphertext =
            encrypt_legacy_for_public_key(E2EE_ALGO_LEGACY_ED25519, &public_hex, b"legacy", None)
                .unwrap();
        assert_eq!(
            decrypt_legacy_ed25519_with_secret_key(&secret, &ciphertext, None).unwrap(),
            b"legacy"
        );
    }
}
