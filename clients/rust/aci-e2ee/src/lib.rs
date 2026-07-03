//! Client-side ACI end-to-end encryption (ACI spec §7).
//!
//! An ACI client encrypts each content-bearing request field to the attested
//! service E2EE key, so the plaintext is readable only inside the TEE even
//! when TLS terminates elsewhere. This crate produces the wire ciphertext for
//! one field and the AES-GCM associated data (AAD) that binds it to its
//! location and request context.
//!
//! Both cipher suites of §7.1 are supported; the client selects one by the
//! `algo` of the keyset entry it encrypts to:
//!
//! * [`ALGO_X25519`] — X25519 + HKDF-SHA256 + AES-256-GCM (RECOMMENDED).
//! * [`ALGO_SECP256K1`] — secp256k1 + HKDF-SHA256 + AES-256-GCM.
//!
//! Each field value on the wire is the lowercase hex of
//! `ephemeral_public_key || aes_gcm_nonce(12) || ciphertext || tag(16)`, with a
//! fresh ephemeral key and GCM nonce per field.
//!
//! # Example
//!
//! ```no_run
//! # use aci_e2ee::{encrypt_request_field, ALGO_X25519};
//! // From the attested keyset entry you selected (`X-Model-Pub-Key`):
//! let service_key_hex = "aa...";
//! // Encrypt the first message's whole content (spec §7.2 field path):
//! let ciphertext_hex = encrypt_request_field(
//!     service_key_hex,
//!     ALGO_X25519,
//!     "gpt-x",                // request `model`, byte-exact
//!     "messages.0.content",   // field path
//!     "6e6f6e63652d31323334", // X-E2EE-Nonce
//!     1_750_000_000,          // X-E2EE-Timestamp
//!     b"hello",
//! ).unwrap();
//! // Put `ciphertext_hex` back at messages[0].content and send with the
//! // X-E2EE-* headers.
//! ```

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use k256::ecdh::diffie_hellman;
use k256::elliptic_curve::sec1::ToEncodedPoint;
use k256::{EncodedPoint, PublicKey as K256PublicKey, SecretKey as K256SecretKey};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret as X25519StaticSecret};

/// X25519 cipher suite identifier (spec §7.1, RECOMMENDED).
pub const ALGO_X25519: &str = "x25519-aes-256-gcm-hkdf-sha256";
/// secp256k1 cipher suite identifier (spec §7.1).
pub const ALGO_SECP256K1: &str = "secp256k1-aes-256-gcm-hkdf-sha256";

const HKDF_INFO_X25519: &[u8] = b"aci.e2ee.v2.x25519";
const HKDF_INFO_SECP256K1: &[u8] = b"aci.e2ee.v2.secp256k1";
const NONCE_LEN: usize = 12;

/// A reason encryption or a public-key parse failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `algo` is not one of the ACI v2 suites.
    UnsupportedAlgo(String),
    /// The recipient public key is not valid hex or not a valid curve point.
    InvalidPublicKey(String),
    /// AES-GCM sealing failed (should not happen for valid inputs).
    Encrypt,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::UnsupportedAlgo(a) => write!(f, "unsupported E2EE algo: {a}"),
            Error::InvalidPublicKey(m) => write!(f, "invalid public key: {m}"),
            Error::Encrypt => write!(f, "E2EE encryption failed"),
        }
    }
}

impl std::error::Error for Error {}

/// Encrypt one request field and return its wire ciphertext (lowercase hex).
///
/// `field` is the location's field path (spec §7.2), e.g. `messages.0.content`,
/// `messages.1.content.0.image_url.url`, `prompt`, or `input.2`. `model` is the
/// request's top-level `model`, byte-exact. `nonce` / `timestamp` are the
/// `X-E2EE-Nonce` / `X-E2EE-Timestamp` you send with the request.
pub fn encrypt_request_field(
    service_public_key_hex: &str,
    algo: &str,
    model: &str,
    field: &str,
    nonce: &str,
    timestamp: u64,
    plaintext: &[u8],
) -> Result<String, Error> {
    let aad = request_aad(algo, model, field, nonce, timestamp);
    encrypt(service_public_key_hex, algo, plaintext, &aad)
}

/// Encrypt `plaintext` to `service_public_key_hex` under `algo` with the given
/// `aad`. Use [`request_aad`] / [`response_aad`] to build `aad`, or
/// [`encrypt_request_field`] for the common request case.
pub fn encrypt(
    service_public_key_hex: &str,
    algo: &str,
    plaintext: &[u8],
    aad: &[u8],
) -> Result<String, Error> {
    let mut gcm_nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut gcm_nonce);
    match algo {
        ALGO_X25519 => {
            let mut eph_secret = [0u8; 32];
            OsRng.fill_bytes(&mut eph_secret);
            seal_x25519(service_public_key_hex, eph_secret, gcm_nonce, plaintext, aad)
        }
        ALGO_SECP256K1 => {
            let eph = K256SecretKey::random(&mut OsRng);
            seal_secp256k1(service_public_key_hex, &eph, gcm_nonce, plaintext, aad)
        }
        other => Err(Error::UnsupportedAlgo(other.to_string())),
    }
}

/// The request-field AAD (spec §7.3): the JCS canonicalization of the
/// purpose-tagged object bound into AES-GCM.
pub fn request_aad(algo: &str, model: &str, field: &str, nonce: &str, timestamp: u64) -> Vec<u8> {
    canonical_object(&[
        ("algo", Scalar::Str(algo)),
        ("field", Scalar::Str(field)),
        ("model", Scalar::Str(model)),
        ("nonce", Scalar::Str(nonce)),
        ("purpose", Scalar::Str("aci.e2ee.request.v2")),
        ("ts", Scalar::Int(timestamp)),
    ])
}

/// The response-field AAD (spec §7.3): like [`request_aad`] but tagged
/// `aci.e2ee.response.v2` and additionally binding the response `id` (`""`
/// when the response carries none). Use the values from your own request for
/// `algo` / `model` / `nonce` / `timestamp`; the service derives the same AAD.
pub fn response_aad(
    algo: &str,
    model: &str,
    id: &str,
    field: &str,
    nonce: &str,
    timestamp: u64,
) -> Vec<u8> {
    canonical_object(&[
        ("algo", Scalar::Str(algo)),
        ("field", Scalar::Str(field)),
        ("id", Scalar::Str(id)),
        ("model", Scalar::Str(model)),
        ("nonce", Scalar::Str(nonce)),
        ("purpose", Scalar::Str("aci.e2ee.response.v2")),
        ("ts", Scalar::Int(timestamp)),
    ])
}

fn seal_x25519(
    recipient_hex: &str,
    ephemeral_secret: [u8; 32],
    gcm_nonce: [u8; NONCE_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<String, Error> {
    let recipient = parse_x25519_public(recipient_hex)?;
    let secret = X25519StaticSecret::from(ephemeral_secret);
    let ephemeral_public = X25519PublicKey::from(&secret);
    let shared = secret.diffie_hellman(&recipient);
    let cipher = aes_from_shared(shared.as_bytes(), HKDF_INFO_X25519)?;
    let ciphertext = seal(&cipher, &gcm_nonce, plaintext, aad)?;
    Ok(pack(ephemeral_public.as_bytes(), &gcm_nonce, &ciphertext))
}

fn seal_secp256k1(
    recipient_hex: &str,
    ephemeral: &K256SecretKey,
    gcm_nonce: [u8; NONCE_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<String, Error> {
    let recipient = parse_secp256k1_public(recipient_hex)?;
    let shared = diffie_hellman(ephemeral.to_nonzero_scalar(), recipient.as_affine());
    let cipher = aes_from_shared(shared.raw_secret_bytes().as_ref(), HKDF_INFO_SECP256K1)?;
    let ciphertext = seal(&cipher, &gcm_nonce, plaintext, aad)?;
    let ephemeral_public = ephemeral.public_key().to_encoded_point(false);
    Ok(pack(ephemeral_public.as_bytes(), &gcm_nonce, &ciphertext))
}

fn seal(
    cipher: &Aes256Gcm,
    gcm_nonce: &[u8; NONCE_LEN],
    plaintext: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>, Error> {
    cipher
        .encrypt(Nonce::from_slice(gcm_nonce), Payload { msg: plaintext, aad })
        .map_err(|_| Error::Encrypt)
}

fn pack(ephemeral_public: &[u8], gcm_nonce: &[u8], ciphertext: &[u8]) -> String {
    let mut blob = Vec::with_capacity(ephemeral_public.len() + gcm_nonce.len() + ciphertext.len());
    blob.extend_from_slice(ephemeral_public);
    blob.extend_from_slice(gcm_nonce);
    blob.extend_from_slice(ciphertext);
    hex::encode(blob)
}

fn aes_from_shared(shared: &[u8], info: &[u8]) -> Result<Aes256Gcm, Error> {
    let hk = Hkdf::<Sha256>::new(None, shared);
    let mut key = [0u8; 32];
    hk.expand(info, &mut key).map_err(|_| Error::Encrypt)?;
    Ok(Aes256Gcm::new_from_slice(&key).expect("AES-256 key length is fixed"))
}

fn parse_x25519_public(value: &str) -> Result<X25519PublicKey, Error> {
    let bytes = decode_hex(value)?;
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| Error::InvalidPublicKey(format!("X25519 key must be 32 bytes, got {}", bytes.len())))?;
    Ok(X25519PublicKey::from(bytes))
}

fn parse_secp256k1_public(value: &str) -> Result<K256PublicKey, Error> {
    let bytes = decode_hex(value)?;
    // Accept the 65-byte uncompressed SEC1 form and the 64-byte form without
    // the 0x04 prefix (spec §7.1).
    let encoded = match bytes.as_slice() {
        [0x04, rest @ ..] if rest.len() == 64 => EncodedPoint::from_bytes([&[0x04], rest].concat()),
        rest if rest.len() == 64 => EncodedPoint::from_bytes([&[0x04], rest].concat()),
        _ => {
            return Err(Error::InvalidPublicKey(format!(
                "secp256k1 key must be 64 or 65 bytes, got {}",
                bytes.len()
            )))
        }
    }
    .map_err(|e| Error::InvalidPublicKey(e.to_string()))?;
    K256PublicKey::from_sec1_bytes(encoded.as_bytes()).map_err(|e| Error::InvalidPublicKey(e.to_string()))
}

fn decode_hex(value: &str) -> Result<Vec<u8>, Error> {
    hex::decode(value.strip_prefix("0x").unwrap_or(value))
        .map_err(|e| Error::InvalidPublicKey(e.to_string()))
}

/// A JSON scalar in an ACI AAD object: a string or an integer.
enum Scalar<'a> {
    Str(&'a str),
    Int(u64),
}

/// Emit the JCS canonicalization of a flat object of string / integer scalars
/// (RFC 8785, the subset ACI AADs use). Keys are sorted by UTF-16 code unit;
/// strings are minimally escaped. We sort here rather than trusting any map
/// order, so the output is identical regardless of the caller's JSON library.
fn canonical_object(entries: &[(&str, Scalar)]) -> Vec<u8> {
    let mut sorted: Vec<&(&str, Scalar)> = entries.iter().collect();
    sorted.sort_by(|a, b| utf16_cmp(a.0, b.0));

    let mut out = String::from("{");
    for (i, (key, value)) in sorted.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        write_json_string(key, &mut out);
        out.push(':');
        match value {
            Scalar::Str(s) => write_json_string(s, &mut out),
            Scalar::Int(n) => out.push_str(&n.to_string()),
        }
    }
    out.push('}');
    out.into_bytes()
}

/// JSON string escaping per RFC 8785 §3.2.2.2: short escapes where defined,
/// `\u00xx` for other control characters, UTF-8 verbatim otherwise.
fn write_json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{09}' => out.push_str("\\t"),
            '\u{0A}' => out.push_str("\\n"),
            '\u{0C}' => out.push_str("\\f"),
            '\u{0D}' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Lexicographic comparison over UTF-16 code units (RFC 8785 §3.2.3).
fn utf16_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

#[cfg(test)]
mod tests {
    use super::*;

    // spec/test-vectors.md §7 — byte-exact expected AAD.
    const REQUEST_AAD_VECTOR: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"messages.0.content","model":"demo-model","nonce":"6e6f6e63652d31323334","purpose":"aci.e2ee.request.v2","ts":1750000000}"#;
    const RESPONSE_AAD_VECTOR: &str = r#"{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"choices.0.message.content","id":"chatcmpl-123","model":"demo-model","nonce":"6e6f6e63652d31323334","purpose":"aci.e2ee.response.v2","ts":1750000000}"#;

    // Fixed inputs for a deterministic known-answer test, shared byte-for-byte
    // with the TypeScript client so the two implementations are proven to
    // interoperate. Do not use fixed keys/nonces in production.
    const EPH_SECRET: [u8; 32] = [1u8; 32];
    const RECIPIENT_SECRET: [u8; 32] = [2u8; 32];
    const GCM_NONCE: [u8; NONCE_LEN] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    const KAT_MODEL: &str = "demo-model";
    const KAT_FIELD: &str = "messages.0.content";
    const KAT_NONCE: &str = "6e6f6e63652d31323334";
    const KAT_TS: u64 = 1_750_000_000;
    const KAT_PLAINTEXT: &[u8] = b"hello";

    // Cross-language known-answer ciphertexts (see clients/typescript).
    const KAT_X25519: &str = "a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209000102030405060708090a0beb61256ee059769140a79f8c2733c7872ba5c6167c";
    const KAT_SECP256K1: &str = "041b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f70beaf8f588b541507fed6a642c5ab42dfdf8120a7f639de5122d47a69a8e8d1000102030405060708090a0bc1efd31f5d94f73340e54c1045b20d4d431f17f277";

    fn x25519_recipient_public_hex(secret: [u8; 32]) -> String {
        let sk = X25519StaticSecret::from(secret);
        hex::encode(X25519PublicKey::from(&sk).as_bytes())
    }

    fn secp256k1_recipient_public_hex(secret: [u8; 32]) -> String {
        let sk = K256SecretKey::from_slice(&secret).unwrap();
        hex::encode(sk.public_key().to_encoded_point(false).as_bytes())
    }

    fn open_x25519(recipient_secret: [u8; 32], blob_hex: &str, aad: &[u8]) -> Vec<u8> {
        let blob = hex::decode(blob_hex).unwrap();
        let eph: [u8; 32] = blob[..32].try_into().unwrap();
        let nonce = &blob[32..44];
        let ct = &blob[44..];
        let sk = X25519StaticSecret::from(recipient_secret);
        let shared = sk.diffie_hellman(&X25519PublicKey::from(eph));
        let cipher = aes_from_shared(shared.as_bytes(), HKDF_INFO_X25519).unwrap();
        cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
            .unwrap()
    }

    fn open_secp256k1(recipient_secret: [u8; 32], blob_hex: &str, aad: &[u8]) -> Vec<u8> {
        let blob = hex::decode(blob_hex).unwrap();
        let eph = K256PublicKey::from_sec1_bytes(&blob[..65]).unwrap();
        let nonce = &blob[65..77];
        let ct = &blob[77..];
        let sk = K256SecretKey::from_slice(&recipient_secret).unwrap();
        let shared = diffie_hellman(sk.to_nonzero_scalar(), eph.as_affine());
        let cipher = aes_from_shared(shared.raw_secret_bytes().as_ref(), HKDF_INFO_SECP256K1).unwrap();
        cipher
            .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
            .unwrap()
    }

    #[test]
    fn request_aad_matches_spec_vector() {
        let aad = request_aad(
            ALGO_X25519,
            "demo-model",
            "messages.0.content",
            "6e6f6e63652d31323334",
            1_750_000_000,
        );
        assert_eq!(aad, REQUEST_AAD_VECTOR.as_bytes());
    }

    #[test]
    fn response_aad_matches_spec_vector() {
        let aad = response_aad(
            ALGO_X25519,
            "demo-model",
            "chatcmpl-123",
            "choices.0.message.content",
            "6e6f6e63652d31323334",
            1_750_000_000,
        );
        assert_eq!(aad, RESPONSE_AAD_VECTOR.as_bytes());
    }

    #[test]
    fn x25519_round_trip() {
        let recipient = x25519_recipient_public_hex(RECIPIENT_SECRET);
        let field = "messages.0.content";
        let blob = encrypt_request_field(
            &recipient,
            ALGO_X25519,
            "gpt-x",
            field,
            "nonce-abc",
            1_700_000_000,
            b"secret prompt",
        )
        .unwrap();
        let aad = request_aad(ALGO_X25519, "gpt-x", field, "nonce-abc", 1_700_000_000);
        assert_eq!(open_x25519(RECIPIENT_SECRET, &blob, &aad), b"secret prompt");
    }

    #[test]
    fn secp256k1_round_trip() {
        let recipient = secp256k1_recipient_public_hex(RECIPIENT_SECRET);
        let field = "prompt";
        let blob =
            encrypt_request_field(&recipient, ALGO_SECP256K1, "gpt-x", field, "nonce-xyz", 42, b"hi")
                .unwrap();
        let aad = request_aad(ALGO_SECP256K1, "gpt-x", field, "nonce-xyz", 42);
        assert_eq!(open_secp256k1(RECIPIENT_SECRET, &blob, &aad), b"hi");
    }

    #[test]
    fn secp256k1_accepts_64_byte_key_without_prefix() {
        let full = secp256k1_recipient_public_hex(RECIPIENT_SECRET);
        let without_prefix = full.trim_start_matches("04");
        let aad = request_aad(ALGO_SECP256K1, "m", "prompt", "n", 1);
        let blob = encrypt(without_prefix, ALGO_SECP256K1, b"x", &aad).unwrap();
        assert_eq!(open_secp256k1(RECIPIENT_SECRET, &blob, &aad), b"x");
    }

    // Deterministic cross-language vectors. Prints the values so they can be
    // pinned in the TypeScript test; asserts once the constants are filled in.
    #[test]
    fn known_answer_vectors() {
        let aad = request_aad(ALGO_X25519, KAT_MODEL, KAT_FIELD, KAT_NONCE, KAT_TS);
        let x = seal_x25519(
            &x25519_recipient_public_hex(RECIPIENT_SECRET),
            EPH_SECRET,
            GCM_NONCE,
            KAT_PLAINTEXT,
            &aad,
        )
        .unwrap();
        let aad2 = request_aad(ALGO_SECP256K1, KAT_MODEL, KAT_FIELD, KAT_NONCE, KAT_TS);
        let s = seal_secp256k1(
            &secp256k1_recipient_public_hex(RECIPIENT_SECRET),
            &K256SecretKey::from_slice(&EPH_SECRET).unwrap(),
            GCM_NONCE,
            KAT_PLAINTEXT,
            &aad2,
        )
        .unwrap();
        println!("KAT_X25519={x}");
        println!("KAT_SECP256K1={s}");
        // Self-consistency regardless of the pinned constants:
        assert_eq!(open_x25519(RECIPIENT_SECRET, &x, &aad), KAT_PLAINTEXT);
        assert_eq!(open_secp256k1(RECIPIENT_SECRET, &s, &aad2), KAT_PLAINTEXT);
        if KAT_SECP256K1 != "PLACEHOLDER" {
            assert_eq!(x, KAT_X25519, "x25519 KAT drift");
            assert_eq!(s, KAT_SECP256K1, "secp256k1 KAT drift");
        }
    }
}
