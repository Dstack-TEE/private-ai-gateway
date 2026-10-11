//! Policy-neutral ACI verification mechanisms.
//!
//! Private AI Gateway and Private AI Proxy each own a relying-party
//! appraisal: which checks run, what a result means, and which trust anchors
//! are acceptable. This crate holds only the mechanisms beneath those
//! decisions — report binding, the quote's report-data slot, dstack event-log
//! replay and measurements, the KMS custody chain, and the declared TLS
//! selection — so both verifiers compute them the same way. Hardware quote
//! verification stays with each verifier; functions here take the fields of
//! a quote the caller has already parsed.

pub mod channel;
pub mod dstack;
pub mod quote;
pub mod report;

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(value).map_err(|e| e.to_string())
}

/// Decode 32 bytes of hex, with or without a `0x` prefix.
pub fn decode_hex_32(value: &str) -> Result<[u8; 32], String> {
    let bytes = decode_hex(value)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("expected 32 bytes, got {}", bytes.len()))
}
