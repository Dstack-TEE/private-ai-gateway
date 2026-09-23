//! Policy-neutral ACI verification mechanisms.
//!
//! This crate contains parsing, cryptographic binding checks, measurement
//! replay, and TLS SPKI helpers shared by the Private AI Gateway and Private
//! AI Proxy. Relying-party policy and appraisal decisions remain in each
//! consumer.

pub mod dstack;
pub mod quote;
pub mod report;
pub mod tls;

fn decode_hex(value: &str) -> Result<Vec<u8>, String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    hex::decode(value).map_err(|error| error.to_string())
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], String> {
    let bytes = decode_hex(value)?;
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| format!("expected 32 bytes, got {}", bytes.len()))
}
