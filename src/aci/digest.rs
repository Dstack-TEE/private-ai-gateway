//! Digest helpers shared across the ACI surface.
//!
//! Every content id and standalone digest field in ACI (§3) is
//! `"sha256:" || lowercase-hex` over the exact bytes named. There is no
//! canonical JSON form anywhere in the protocol: the bytes are the artifact.

use sha2::{Digest, Sha256};

/// `"sha256:" || hex(sha256(payload))` — the §3 content-id/digest format.
pub fn sha256_hex(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(payload)))
}

/// Raw 32-byte SHA-256 of `payload`.
pub fn sha256_raw(payload: &[u8]) -> [u8; 32] {
    Sha256::digest(payload).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vector() {
        assert_eq!(
            sha256_hex(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_raw_matches_hex_form() {
        let raw = sha256_raw(b"abc");
        assert_eq!(sha256_hex(b"abc"), format!("sha256:{}", hex::encode(raw)));
    }
}
