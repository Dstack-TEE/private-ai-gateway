//! ACI and dstack-vllm-proxy E2EE helpers.
//!
//! ACI v1 (§7.1) defines two cipher suites — X25519 (RECOMMENDED,
//! browser-native) and secp256k1 (EVM/dstack-native). Both do ECDH to a
//! fresh ephemeral key, HKDF-SHA256, and AES-256-GCM; they differ only in
//! the curve, the ephemeral key encoding, and the HKDF `info` string. The
//! wire ciphertext of either suite is:
//!
//! ```text
//! ephemeral_public_key || aes_gcm_nonce || ciphertext_tag
//! ```
//!
//! encoded as lowercase hex, where the ephemeral key is 32 raw bytes for
//! X25519 and the 65-byte uncompressed SEC1 point for secp256k1. The AAD
//! strings are built by the caller from ACI §7.3.
//!
//! The inherited dstack-vllm-proxy profile uses the legacy `ecdsa`
//! and `ed25519` labels, different HKDF context strings, and v1
//! payloads with no AAD.

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

pub const E2EE_VERSION_V2: &str = "2";
pub const E2EE_VERSION_V1: &str = "1";
pub const E2EE_ALGO_X25519_AESGCM: &str = "x25519-aes-256-gcm-hkdf-sha256";
pub const E2EE_ALGO_SECP256K1_AESGCM: &str = "secp256k1-aes-256-gcm-hkdf-sha256";
pub const E2EE_ALGO_LEGACY_ECDSA: &str = "ecdsa";
pub const E2EE_ALGO_LEGACY_ED25519: &str = "ed25519";

const SECP256K1_HKDF_INFO: &[u8] = b"aci.e2ee.v2.secp256k1";
const X25519_HKDF_INFO: &[u8] = b"aci.e2ee.v2.x25519";
const LEGACY_ECDSA_HKDF_INFO: &[u8] = b"ecdsa_encryption";
const LEGACY_ED25519_HKDF_INFO: &[u8] = b"ed25519_encryption";
const PUBLIC_KEY_LEN: usize = 65;
const X25519_PUBLIC_KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;

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

/// True for the two ACI v1 §7.1 E2EE cipher suites. Other keyset `algo`
/// values (legacy labels, unknown suites) are ignored for E2EE selection.
pub fn is_aci_e2ee_suite(algo: &str) -> bool {
    matches!(algo, E2EE_ALGO_X25519_AESGCM | E2EE_ALGO_SECP256K1_AESGCM)
}

/// Normalize a §7.1 public key to canonical lowercase hex for the suite's
/// `algo`: 32-byte raw for X25519, 65-byte uncompressed SEC1 for secp256k1.
pub fn normalize_aci_e2ee_public_key_hex(algo: &str, value: &str) -> Result<String, KeyError> {
    match algo {
        E2EE_ALGO_X25519_AESGCM => normalize_x25519_public_key_hex(value),
        E2EE_ALGO_SECP256K1_AESGCM => normalize_secp256k1_public_key_hex(value),
        other => Err(KeyError::UnsupportedAlgo(other.to_string())),
    }
}

/// Encrypt one ACI v2 field to a recipient public key under the given §7.1
/// suite. Used for response fields, keyed by the selected service E2EE key's
/// `algo`.
pub fn encrypt_aci_e2ee_for_public_key(
    algo: &str,
    recipient_public_key_hex: &str,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<String, KeyError> {
    match algo {
        E2EE_ALGO_X25519_AESGCM => {
            encrypt_x25519_for_public_key(recipient_public_key_hex, plaintext, aad)
        }
        E2EE_ALGO_SECP256K1_AESGCM => {
            encrypt_for_public_key(recipient_public_key_hex, plaintext, aad)
        }
        other => Err(KeyError::UnsupportedAlgo(other.to_string())),
    }
}

pub fn normalize_x25519_public_key_hex(value: &str) -> Result<String, KeyError> {
    let bytes = hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .map_err(|e| KeyError::Crypto(format!("invalid X25519 public key hex: {e}")))?;
    let bytes: [u8; X25519_PUBLIC_KEY_LEN] = bytes.as_slice().try_into().map_err(|_| {
        KeyError::Crypto(format!(
            "X25519 public key must be 32 bytes, got {}",
            bytes.len()
        ))
    })?;
    Ok(hex::encode(bytes))
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

pub fn encrypt_x25519_for_public_key(
    recipient_public_key_hex: &str,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<String, KeyError> {
    let recipient = x25519_public_key_from_hex(recipient_public_key_hex)?;
    let ephemeral = X25519SecretKey::from(rand::random::<[u8; 32]>());
    let ephemeral_public = X25519PublicKey::from(&ephemeral);
    let shared = ephemeral.diffie_hellman(&recipient);
    let cipher = x25519_cipher_from_shared_secret(shared.as_bytes())?;
    let nonce_bytes: [u8; NONCE_LEN] = rand::random();
    let ciphertext = cipher
        .encrypt(
            &nonce_bytes.into(),
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| KeyError::Crypto(format!("X25519 E2EE encrypt failed: {e}")))?;

    let mut out = Vec::with_capacity(X25519_PUBLIC_KEY_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(ephemeral_public.as_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(hex::encode(out))
}

pub fn decrypt_x25519_with_secret_key(
    recipient_secret: &X25519SecretKey,
    ciphertext_hex: &str,
    aad: &[u8],
) -> Result<Vec<u8>, KeyError> {
    let blob = hex::decode(ciphertext_hex.strip_prefix("0x").unwrap_or(ciphertext_hex))
        .map_err(|e| KeyError::Crypto(format!("invalid X25519 E2EE ciphertext hex: {e}")))?;
    if blob.len() < X25519_PUBLIC_KEY_LEN + NONCE_LEN + TAG_LEN {
        return Err(KeyError::Crypto(format!(
            "X25519 E2EE ciphertext too short: got {} bytes",
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
    let shared = recipient_secret.diffie_hellman(&eph);
    let cipher = x25519_cipher_from_shared_secret(shared.as_bytes())?;
    cipher
        .decrypt(
            &nonce_bytes.into(),
            aes_gcm::aead::Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|e| KeyError::Crypto(format!("X25519 E2EE decrypt failed: {e}")))
}

pub fn encrypt_for_public_key(
    recipient_public_key_hex: &str,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<String, KeyError> {
    let recipient = public_key_from_hex(recipient_public_key_hex)?;
    let ephemeral = SecretKey::random(&mut OsRng);
    let shared = diffie_hellman(ephemeral.to_nonzero_scalar(), recipient.as_affine());
    let shared_bytes = shared.raw_secret_bytes();
    let cipher = cipher_from_shared_secret(shared_bytes.as_ref())?;
    let nonce_bytes: [u8; NONCE_LEN] = rand::random();
    let nonce = nonce_bytes.into();
    let ciphertext = cipher
        .encrypt(
            &nonce,
            aes_gcm::aead::Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|e| KeyError::Crypto(format!("E2EE encrypt failed: {e}")))?;

    let mut out = Vec::with_capacity(PUBLIC_KEY_LEN + NONCE_LEN + ciphertext.len());
    out.extend_from_slice(ephemeral.public_key().to_encoded_point(false).as_bytes());
    out.extend_from_slice(&nonce_bytes);
    out.extend_from_slice(&ciphertext);
    Ok(hex::encode(out))
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

            let mut out = Vec::with_capacity(PUBLIC_KEY_LEN + NONCE_LEN + ciphertext.len());
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

pub fn decrypt_with_secret_key(
    recipient_secret: &SecretKey,
    ciphertext_hex: &str,
    aad: &[u8],
) -> Result<Vec<u8>, KeyError> {
    let blob = hex::decode(ciphertext_hex.strip_prefix("0x").unwrap_or(ciphertext_hex))
        .map_err(|e| KeyError::Crypto(format!("invalid E2EE ciphertext hex: {e}")))?;
    if blob.len() < PUBLIC_KEY_LEN + NONCE_LEN + TAG_LEN {
        return Err(KeyError::Crypto(format!(
            "E2EE ciphertext too short: got {} bytes",
            blob.len()
        )));
    }
    let eph = PublicKey::from_sec1_bytes(&blob[..PUBLIC_KEY_LEN])
        .map_err(|e| KeyError::Crypto(format!("invalid E2EE ephemeral public key: {e}")))?;
    let nonce_bytes: [u8; NONCE_LEN] = blob[PUBLIC_KEY_LEN..PUBLIC_KEY_LEN + NONCE_LEN]
        .try_into()
        .expect("nonce length is checked");
    let nonce = nonce_bytes.into();
    let ciphertext = &blob[PUBLIC_KEY_LEN + NONCE_LEN..];
    let shared = diffie_hellman(recipient_secret.to_nonzero_scalar(), eph.as_affine());
    let shared_bytes = shared.raw_secret_bytes();
    let cipher = cipher_from_shared_secret(shared_bytes.as_ref())?;
    cipher
        .decrypt(
            &nonce,
            aes_gcm::aead::Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|e| KeyError::Crypto(format!("E2EE decrypt failed: {e}")))
}

pub fn decrypt_legacy_ecdsa_with_secret_key(
    recipient_secret: &SecretKey,
    ciphertext_hex: &str,
    aad: Option<&[u8]>,
) -> Result<Vec<u8>, KeyError> {
    let blob = hex::decode(ciphertext_hex.strip_prefix("0x").unwrap_or(ciphertext_hex))
        .map_err(|e| KeyError::Crypto(format!("invalid legacy E2EE ciphertext hex: {e}")))?;
    if blob.len() < PUBLIC_KEY_LEN + NONCE_LEN + TAG_LEN {
        return Err(KeyError::Crypto(format!(
            "legacy ECDSA E2EE ciphertext too short: got {} bytes",
            blob.len()
        )));
    }
    let eph = PublicKey::from_sec1_bytes(&blob[..PUBLIC_KEY_LEN])
        .map_err(|e| KeyError::Crypto(format!("invalid legacy ECDSA ephemeral public key: {e}")))?;
    let nonce_bytes: [u8; NONCE_LEN] = blob[PUBLIC_KEY_LEN..PUBLIC_KEY_LEN + NONCE_LEN]
        .try_into()
        .expect("nonce length is checked");
    let ciphertext = &blob[PUBLIC_KEY_LEN + NONCE_LEN..];
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

fn cipher_from_shared_secret(shared: &[u8]) -> Result<Aes256Gcm, KeyError> {
    let hk = Hkdf::<Sha256>::new(None, shared);
    let mut key = [0u8; 32];
    hk.expand(SECP256K1_HKDF_INFO, &mut key)
        .map_err(|e| KeyError::Crypto(format!("HKDF expand failed: {e}")))?;
    Ok(Aes256Gcm::new_from_slice(&key).expect("AES-256 key length is fixed"))
}

fn x25519_public_key_from_hex(value: &str) -> Result<X25519PublicKey, KeyError> {
    let normalized = normalize_x25519_public_key_hex(value)?;
    let bytes: [u8; X25519_PUBLIC_KEY_LEN] = hex::decode(normalized)
        .expect("normalized X25519 hex decodes")
        .as_slice()
        .try_into()
        .expect("normalized X25519 key is 32 bytes");
    Ok(X25519PublicKey::from(bytes))
}

fn x25519_cipher_from_shared_secret(shared: &[u8]) -> Result<Aes256Gcm, KeyError> {
    let hk = Hkdf::<Sha256>::new(None, shared);
    let mut key = [0u8; 32];
    hk.expand(X25519_HKDF_INFO, &mut key)
        .map_err(|e| KeyError::Crypto(format!("HKDF expand failed: {e}")))?;
    Ok(Aes256Gcm::new_from_slice(&key).expect("AES-256 key length is fixed"))
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

    fn x25519_recipient() -> (X25519SecretKey, String) {
        let secret = x25519_secret_key_from_bytes(&[0x37u8; 32]).unwrap();
        let public_hex = x25519_public_key_hex(&secret);
        (secret, public_hex)
    }

    #[test]
    fn x25519_round_trip_recovers_plaintext() {
        let (secret, public_hex) = x25519_recipient();
        let aad = b"aci.e2ee.request.v2 aad";
        let ciphertext = encrypt_x25519_for_public_key(&public_hex, b"hello x25519", aad).unwrap();
        // Wire format: 32-byte ephemeral pub || 12-byte nonce || ct||tag.
        let blob = hex::decode(&ciphertext).unwrap();
        assert!(blob.len() > X25519_PUBLIC_KEY_LEN + NONCE_LEN + TAG_LEN);
        let plaintext = decrypt_x25519_with_secret_key(&secret, &ciphertext, aad).unwrap();
        assert_eq!(plaintext, b"hello x25519");
    }

    #[test]
    fn x25519_decrypt_rejects_wrong_aad() {
        let (secret, public_hex) = x25519_recipient();
        let ciphertext = encrypt_x25519_for_public_key(&public_hex, b"bound", b"aad-a").unwrap();
        assert!(decrypt_x25519_with_secret_key(&secret, &ciphertext, b"aad-b").is_err());
    }

    #[test]
    fn x25519_public_key_accepts_optional_0x_prefix() {
        let (_secret, public_hex) = x25519_recipient();
        let prefixed = format!("0x{public_hex}");
        assert_eq!(
            normalize_x25519_public_key_hex(&prefixed).unwrap(),
            public_hex
        );
    }

    #[test]
    fn suite_dispatch_routes_by_algo() {
        let (secret, public_hex) = x25519_recipient();
        let aad = b"aad";
        let ciphertext =
            encrypt_aci_e2ee_for_public_key(E2EE_ALGO_X25519_AESGCM, &public_hex, b"msg", aad)
                .unwrap();
        assert_eq!(
            decrypt_x25519_with_secret_key(&secret, &ciphertext, aad).unwrap(),
            b"msg"
        );
        assert!(is_aci_e2ee_suite(E2EE_ALGO_X25519_AESGCM));
        assert!(is_aci_e2ee_suite(E2EE_ALGO_SECP256K1_AESGCM));
        assert!(!is_aci_e2ee_suite(E2EE_ALGO_LEGACY_ED25519));
    }
}
