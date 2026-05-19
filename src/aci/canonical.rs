//! Canonical JSON for ACI digests.
//!
//! ACI digests every protocol object - workload identity, workload
//! keyset, attestation statement, keyset endorsement payload, and
//! receipt - through a canonical JSON serialization. The goal is the
//! same as RFC 8785 JCS: two independent implementations produce the
//! same bytes for the same logical value.
//!
//! This module is **not** a general RFC 8785 implementation. It
//! implements the subset of JCS that ACI wire shapes need:
//!
//! * `String`, `i64`, `bool`, `null`,
//! * arrays of accepted values (declared order),
//! * objects whose keys are strings, whose values are accepted
//!   values, and whose keys are emitted in UTF-16 code-unit order.
//!
//! Floats are **rejected** with [`CanonicalError::FloatNotAllowed`].
//! RFC 8785 §3.2.2.3 requires ECMAScript-262 number serialization for
//! non-integer numerics, which a small ad-hoc implementation cannot
//! produce faithfully. Silently accepting a `f64` here would give an
//! output that disagrees with a conformant JCS implementation. ACI
//! itself defines only integer numerics (epoch version, expiry
//! timestamps, byte counts), so the rejected case never occurs on a
//! conformant ACI object.
//!
//! Object keys are sorted by UTF-16 code units (RFC 8785 §3.2.3) by
//! re-encoding each key to a `Vec<u16>` and comparing lexicographically.
//! For ASCII this matches `str::cmp`; for supplementary-plane Unicode
//! characters it does not, and ACI fields stay inside the Basic
//! Multilingual Plane today.

use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

/// Reason a value cannot be canonicalised.
#[derive(Debug, thiserror::Error)]
pub enum CanonicalError {
    /// A `f64` or other non-integer number entered the value graph.
    #[error(
        "JCS: float / non-integer numeric is not allowed in the ACI \
         value space (RFC 8785 §3.2.2.3 number canonicalisation would \
         require ECMAScript-262 serialisation)"
    )]
    FloatNotAllowed,
    /// An object key is not a string. JSON forbids this; the check is
    /// defensive in case a caller constructs a `serde_json::Value`
    /// through a non-standard path.
    #[error("JCS: object key must be a string")]
    NonStringKey,
}

/// Canonicalise an arbitrary [`serde_json::Value`] subtree.
pub fn canonicalize(value: &Value) -> Result<Vec<u8>, CanonicalError> {
    let mut out = Vec::with_capacity(64);
    write_value(&mut out, value)?;
    Ok(out)
}

/// Canonicalise then return `"sha256:" || hex(sha256(JCS(value)))`.
pub fn jcs_sha256_hex(value: &Value) -> Result<String, CanonicalError> {
    let bytes = canonicalize(value)?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(&bytes))))
}

/// Canonicalise then return the raw 32-byte digest of `sha256(JCS(value))`.
pub fn jcs_sha256_raw(value: &Value) -> Result<[u8; 32], CanonicalError> {
    let bytes = canonicalize(value)?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(&bytes));
    Ok(out)
}

/// Return `"sha256:" || hex(sha256(payload))` from raw bytes.
pub fn sha256_hex(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(payload)))
}

fn write_value(out: &mut Vec<u8>, value: &Value) -> Result<(), CanonicalError> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => write_number(out, n)?,
        Value::String(s) => write_string(out, s),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(out, item)?;
            }
            out.push(b']');
        }
        Value::Object(map) => write_object(out, map)?,
    }
    Ok(())
}

fn write_number(out: &mut Vec<u8>, n: &Number) -> Result<(), CanonicalError> {
    if let Some(i) = n.as_i64() {
        out.extend_from_slice(i.to_string().as_bytes());
        return Ok(());
    }
    if let Some(u) = n.as_u64() {
        out.extend_from_slice(u.to_string().as_bytes());
        return Ok(());
    }
    Err(CanonicalError::FloatNotAllowed)
}

fn write_object(out: &mut Vec<u8>, map: &Map<String, Value>) -> Result<(), CanonicalError> {
    // Sort object keys by UTF-16 code unit sequence (RFC 8785 §3.2.3).
    // Collect first to avoid mutating the input map.
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by(|a, b| utf16_compare(a, b));

    out.push(b'{');
    for (i, key) in keys.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write_string(out, key);
        out.push(b':');
        // `map` is `serde_json::Map`, keyed by `String`, so non-string keys
        // cannot occur through serde_json; the variant exists for defensive
        // symmetry only.
        let value = map.get(key.as_str()).ok_or(CanonicalError::NonStringKey)?;
        write_value(out, value)?;
    }
    out.push(b'}');
    Ok(())
}

/// Lexicographic compare on the UTF-16 code unit sequence of `a` and `b`.
fn utf16_compare(a: &str, b: &str) -> std::cmp::Ordering {
    let mut ia = a.encode_utf16();
    let mut ib = b.encode_utf16();
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) => match x.cmp(&y) {
                std::cmp::Ordering::Equal => continue,
                other => return other,
            },
        }
    }
}

/// JSON string serialiser matching RFC 8785 §3.2.2.2. ASCII control
/// characters use the short escapes when available (`\b`, `\f`, `\n`,
/// `\r`, `\t`) or `\u00XX` otherwise. Non-ASCII characters emit their
/// UTF-8 bytes verbatim.
fn write_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{0008}' => out.extend_from_slice(b"\\b"),
            '\u{0009}' => out.extend_from_slice(b"\\t"),
            '\u{000A}' => out.extend_from_slice(b"\\n"),
            '\u{000C}' => out.extend_from_slice(b"\\f"),
            '\u{000D}' => out.extend_from_slice(b"\\r"),
            c if (c as u32) < 0x20 => {
                let buf = format!("\\u{:04x}", c as u32);
                out.extend_from_slice(buf.as_bytes());
            }
            c => {
                let mut buf = [0u8; 4];
                let s = c.encode_utf8(&mut buf);
                out.extend_from_slice(s.as_bytes());
            }
        }
    }
    out.push(b'"');
}
