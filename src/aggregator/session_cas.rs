//! Field-level content-addressed packing for attested-session documents.
//!
//! A session document is ~99% repeated bytes across re-verification rounds of
//! the same upstream (see docs/aci-session-storage-study.md): the evidence
//! embeds an internal duplicate (`all_attestations[0]` mirrors the top level),
//! large fields (app_compose, app_cert, event_log, GPU certificates) change
//! only on deployment events, and only the quote / GPU evidence / nonce are
//! new each round. This module splits a sealed document into a small
//! per-session **skeleton** plus content-addressed **chunks** that dedup
//! across rounds, upstreams, and inside one document.
//!
//! The rules are generic — no provider schema:
//!
//! * R0 `evidence.data` data URIs are decoded; a compact-JSON payload is
//!   recursed into, anything else becomes a binary chunk.
//! * R1 strings that are themselves compact JSON (byte-exact re-encode, both
//!   UTF-8 and ASCII-escaped flavors accepted) are recursed into.
//! * R2/R3 hex and base64 strings are stored as decoded binary chunks.
//! * R4 any other large string becomes a raw chunk.
//! * R6 any processed subtree whose canonical form exceeds the threshold
//!   becomes a chunk (merkle-ized document), deduping the internal duplicate
//!   and shared subtrees.
//!
//! Every rule is verified byte-exact at pack time: packing immediately
//! rebuilds the document and compares it to the input. Any deviation — an
//! unfamiliar encoding, an unexpected producer format, a future upstream
//! changing its evidence shape — falls back to [`Packed::Whole`], which
//! stores the document untouched. Rebuild never depends on the rules being
//! right: it is checked against the input, not assumed.
//!
//! Reconstruction invariants (why this is tamper-evident): the rebuilt bytes
//! re-hash to the `session_id` a receipt cites, and `evidence.data` re-hashes
//! to `evidence.digest` (§8.2). Both are verified by the store on every read.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Strings and subtree canonical forms above this size become chunks.
const CHUNK_THRESHOLD: usize = 1024;

/// A packed session document: a small skeleton plus the chunks it references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Packed {
    /// Fallback: the document bytes, untouched. No chunks are referenced.
    Whole(Vec<u8>),
    /// CAS form: skeleton JSON (with `$`-markers) + referenced chunks
    /// (`(sha256-hex, payload)`). Chunks may duplicate what the store already
    /// has; the store writes each digest at most once.
    Cas {
        skeleton: Vec<u8>,
        chunks: Vec<(String, Vec<u8>)>,
    },
}

/// Rebuild the original document bytes from a packed skeleton.
///
/// `whole` selects the fallback form (`skeleton` then holds the document
/// itself). `chunk` resolves a chunk digest to its payload. Returns `None`
/// when a chunk is missing or the skeleton is malformed — the store treats
/// that as corruption, never as a cache miss to silently skip.
pub fn unpack(
    skeleton: &[u8],
    whole: bool,
    chunk: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    if whole {
        return Some(skeleton.to_vec());
    }
    let value: Value = serde_json::from_slice(skeleton).ok()?;
    let rebuilt = rebuild(&value, chunk)?;
    let mut out = String::new();
    write_value(&rebuilt, true, false, &mut out);
    Some(out.into_bytes())
}

/// Pack one sealed session document. Never fails in a way that loses data:
/// any deviation from the expected shapes yields [`Packed::Whole`].
pub fn pack(document: &[u8]) -> Packed {
    match pack_cas(document) {
        Some(packed) => packed,
        None => Packed::Whole(document.to_vec()),
    }
}

fn pack_cas(document: &[u8]) -> Option<Packed> {
    let value: Value = serde_json::from_slice(document).ok()?;
    let mut chunks: Vec<(String, Vec<u8>)> = Vec::new();
    let skeleton_value = process(&value, Mode::Jcs, None, &mut chunks);
    let mut skeleton = String::new();
    write_value(&skeleton_value, true, false, &mut skeleton);

    // Self-check: rebuild from the skeleton with the chunks we just produced
    // and demand the exact input bytes back. Any miss falls back to Whole.
    let mut by_digest = |digest: &str| {
        chunks
            .iter()
            .find(|(d, _)| d == digest)
            .map(|(_, p)| p.clone())
    };
    let rebuilt = unpack(skeleton.as_bytes(), false, &mut by_digest)?;
    if rebuilt != document {
        return None;
    }
    Some(Packed::Cas {
        skeleton: skeleton.into_bytes(),
        chunks,
    })
}

/// Serialization context. The outer document is JCS (sorted); embedded JSON
/// strings keep their producer's key order and one of two escaping flavors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Jcs,
    Compact,
    CompactAscii,
}

impl Mode {
    /// Escaping flavor for canonical forms in this context. Only affects how
    /// subtree-chunk payloads are written (storage bytes), never rebuild
    /// semantics — payloads are parsed back, not compared.
    fn ascii_escape(self) -> bool {
        matches!(self, Mode::CompactAscii)
    }
}

fn process(
    value: &Value,
    mode: Mode,
    key_hint: Option<&str>,
    chunks: &mut Vec<(String, Vec<u8>)>,
) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), process(v, mode, Some(k.as_str()), chunks));
            }
            chunk_subtree(Value::Object(out), mode, chunks)
        }
        Value::Array(items) => {
            let out: Vec<Value> = items
                .iter()
                .map(|v| process(v, mode, None, chunks))
                .collect();
            chunk_subtree(Value::Array(out), mode, chunks)
        }
        Value::String(s) => process_string(s, key_hint, chunks),
        other => other.clone(),
    }
}

/// R6: a processed subtree whose canonical form exceeds the threshold becomes
/// a chunk. The canonical form preserves insertion order — nested structures
/// may carry order-sensitive embedded contexts (a sorting canonicalizer would
/// corrupt them on rebuild).
fn chunk_subtree(value: Value, mode: Mode, chunks: &mut Vec<(String, Vec<u8>)>) -> Value {
    let mut canon = String::new();
    write_value(&value, false, mode.ascii_escape(), &mut canon);
    if canon.len() > CHUNK_THRESHOLD {
        return chunk_ref(canon.into_bytes(), b's', chunks);
    }
    value
}

fn process_string(s: &str, key_hint: Option<&str>, chunks: &mut Vec<(String, Vec<u8>)>) -> Value {
    // R0: evidence.data data URI — decode; recurse a compact-JSON payload.
    if key_hint == Some("data") && s.len() > CHUNK_THRESHOLD {
        if let Some(packed) = try_pack_data_uri(s, chunks) {
            return packed;
        }
    }
    if s.len() <= CHUNK_THRESHOLD {
        return Value::String(s.to_string());
    }
    // R1: embedded compact JSON (byte-exact re-encode in either flavor).
    if s.starts_with('{') || s.starts_with('[') {
        if let Ok(inner) = serde_json::from_str::<Value>(s) {
            let mut utf8 = String::new();
            write_value(&inner, false, false, &mut utf8);
            if utf8 == s {
                return marker("$j", process(&inner, Mode::Compact, None, chunks));
            }
            let mut ascii = String::new();
            write_value(&inner, false, true, &mut ascii);
            if ascii == s {
                return marker("$ja", process(&inner, Mode::CompactAscii, None, chunks));
            }
        }
    }
    // R2: lowercase hex -> binary.
    if s.len().is_multiple_of(2)
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        if let Ok(raw) = hex::decode(s) {
            return chunk_ref(raw, b'h', chunks);
        }
    }
    // R3: standard base64, only when re-encoding reproduces the string exactly.
    if let Ok(raw) = BASE64.decode(s.as_bytes()) {
        if BASE64.encode(&raw) == s {
            return chunk_ref(raw, b'b', chunks);
        }
    }
    // R4: anything else, stored verbatim.
    chunk_ref(s.as_bytes().to_vec(), b'r', chunks)
}

/// R0: pack a `data:<content-type>;base64,<payload>` URI.
fn try_pack_data_uri(s: &str, chunks: &mut Vec<(String, Vec<u8>)>) -> Option<Value> {
    let (head, b64) = s.split_once(";base64,")?;
    let content_type = head.strip_prefix("data:")?;
    let raw = BASE64.decode(b64.as_bytes()).ok()?;
    // Payload that is compact JSON: recurse (both escaping flavors).
    if let Ok(inner) = serde_json::from_slice::<Value>(&raw) {
        if inner.is_object() || inner.is_array() {
            let mut utf8 = String::new();
            write_value(&inner, false, false, &mut utf8);
            if utf8.as_bytes() == raw.as_slice() {
                return Some(data_uri_marker(
                    "$du",
                    content_type,
                    process(&inner, Mode::Compact, None, chunks),
                ));
            }
            let mut ascii = String::new();
            write_value(&inner, false, true, &mut ascii);
            if ascii.as_bytes() == raw.as_slice() {
                return Some(data_uri_marker(
                    "$da",
                    content_type,
                    process(&inner, Mode::CompactAscii, None, chunks),
                ));
            }
        }
    }
    // Opaque payload: one binary chunk.
    let reference = match chunk_ref(raw, b'r', chunks) {
        Value::Object(map) => map.get("$r")?.as_str()?.to_string(),
        _ => return None,
    };
    let mut out = serde_json::Map::new();
    out.insert("$db".to_string(), Value::String(content_type.to_string()));
    out.insert("r".to_string(), Value::String(reference));
    Some(Value::Object(out))
}

fn marker(tag: &str, node: Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert(tag.to_string(), node);
    Value::Object(out)
}

fn data_uri_marker(tag: &str, content_type: &str, node: Value) -> Value {
    let mut out = serde_json::Map::new();
    out.insert(tag.to_string(), Value::String(content_type.to_string()));
    out.insert("v".to_string(), node);
    Value::Object(out)
}

fn chunk_ref(payload: Vec<u8>, tag: u8, chunks: &mut Vec<(String, Vec<u8>)>) -> Value {
    let digest = hex::encode(Sha256::digest(&payload));
    if !chunks.iter().any(|(d, _)| d == &digest) {
        chunks.push((digest.clone(), payload));
    }
    marker("$r", Value::String(format!("{}:{digest}", tag as char)))
}

// ---------------------------------------------------------------------------
// Rebuild
// ---------------------------------------------------------------------------

/// Parse and resolve one `"<tag>:<digest>"` chunk reference. Every step is
/// bounds- and shape-checked: a malformed reference (empty, single-byte,
/// Unicode, wrong tag, non-hex digest) yields `None`, so `pack`'s self-check
/// falls back to `Whole` instead of panicking on unfamiliar producer content.
fn rebuild_reference(
    reference: &str,
    chunk: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    depth: usize,
) -> Option<Value> {
    let tag = reference.get(..2)?;
    let digest = reference.get(2..)?;
    if !is_bare_hex64(digest) {
        return None;
    }
    let payload = chunk(digest)?;
    Some(match tag {
        "h:" => Value::String(hex::encode(payload)),
        "b:" => Value::String(BASE64.encode(payload)),
        "r:" => Value::String(String::from_utf8(payload).ok()?),
        "s:" => {
            let node: Value = serde_json::from_slice(&payload).ok()?;
            return rebuild_at(&node, chunk, depth + 1);
        }
        _ => return None,
    })
}

/// A bare 64-char lowercase-hex id/digest, as produced by content addressing.
pub(crate) fn is_bare_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Recursion ceiling while rebuilding a packed document. Legitimate pack
/// output nests a handful of levels (document -> data URI -> evidence ->
/// subtree chunks); a graph deeper than this is adversarial — fail closed
/// instead of exhausting the stack. Content-addressed chunks additionally
/// make true cycles cryptographically unreachable (see the store's
/// `read_chunk`), so this bound only ever fires on corrupted input.
const MAX_REBUILD_DEPTH: usize = 64;

fn rebuild(value: &Value, chunk: &mut dyn FnMut(&str) -> Option<Vec<u8>>) -> Option<Value> {
    rebuild_at(value, chunk, 0)
}

fn rebuild_at(
    value: &Value,
    chunk: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
    depth: usize,
) -> Option<Value> {
    if depth > MAX_REBUILD_DEPTH {
        return None;
    }
    match value {
        Value::Object(map) => {
            if map.len() == 1 {
                if let Some(reference) = map.get("$r").and_then(Value::as_str) {
                    return rebuild_reference(reference, chunk, depth);
                }
                if let Some(node) = map.get("$j") {
                    let mut s = String::new();
                    write_value(&rebuild_at(node, chunk, depth + 1)?, false, false, &mut s);
                    return Some(Value::String(s));
                }
                if let Some(node) = map.get("$ja") {
                    let mut s = String::new();
                    write_value(&rebuild_at(node, chunk, depth + 1)?, false, true, &mut s);
                    return Some(Value::String(s));
                }
            }
            if map.len() == 2 {
                for (tag, ascii) in [("$du", false), ("$da", true)] {
                    if let (Some(ct), Some(node)) =
                        (map.get(tag).and_then(Value::as_str), map.get("v"))
                    {
                        let mut s = String::new();
                        write_value(&rebuild_at(node, chunk, depth + 1)?, false, ascii, &mut s);
                        return Some(Value::String(format!(
                            "data:{ct};base64,{}",
                            BASE64.encode(s.as_bytes())
                        )));
                    }
                }
                if let (Some(ct), Some(reference)) = (
                    map.get("$db").and_then(Value::as_str),
                    map.get("r").and_then(Value::as_str),
                ) {
                    let digest = reference.get(2..)?; // strip "r:"
                    let payload = chunk(digest)?;
                    return Some(Value::String(format!(
                        "data:{ct};base64,{}",
                        BASE64.encode(payload)
                    )));
                }
            }
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                out.insert(k.clone(), rebuild(v, chunk)?);
            }
            Some(Value::Object(out))
        }
        Value::Array(items) => Some(Value::Array(
            items
                .iter()
                .map(|v| rebuild(v, chunk))
                .collect::<Option<Vec<_>>>()?,
        )),
        other => Some(other.clone()),
    }
}

// ---------------------------------------------------------------------------
// JSON writer: sorted (JCS) or insertion-order, UTF-8 or ASCII-escaped.
// ---------------------------------------------------------------------------

fn write_value(value: &Value, sort_keys: bool, ascii: bool, out: &mut String) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_string(s, ascii, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, sort_keys, ascii, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            out.push('{');
            if sort_keys {
                let mut entries: Vec<(&String, &Value)> = map.iter().collect();
                entries.sort_unstable_by(|a, b| a.0.cmp(b.0));
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(k, ascii, out);
                    out.push(':');
                    write_value(v, sort_keys, ascii, out);
                }
            } else {
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_string(k, ascii, out);
                    out.push(':');
                    write_value(v, sort_keys, ascii, out);
                }
            }
            out.push('}');
        }
    }
}

/// JSON string escaping matching serde_json's control-character style
/// (`\" \\ \n \r \t \b \f`, other controls as `\u00xx` lowercase). With
/// `ascii`, non-ASCII scalars become `\uxxxx` (surrogate pairs beyond the
/// BMP), matching the common `ensure_ascii` producer flavor.
fn write_string(s: &str, ascii: bool, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if ascii && !c.is_ascii() => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{:04x}", unit));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aci::digest;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/sessions/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read(path).expect("fixture readable")
    }

    fn unpack_with_chunks(packed: &Packed) -> Vec<u8> {
        match packed {
            Packed::Whole(bytes) => bytes.clone(),
            Packed::Cas { skeleton, chunks } => {
                let mut by_digest =
                    |d: &str| chunks.iter().find(|(k, _)| k == d).map(|(_, p)| p.clone());
                unpack(skeleton, false, &mut by_digest).expect("rebuild succeeds")
            }
        }
    }

    /// The core accuracy property: every real production document packs and
    /// rebuilds to its exact bytes, so the content address (session id) a
    /// receipt cites still recomputes.
    #[test]
    fn real_documents_roundtrip_byte_exact() {
        for name in [
            "phala-direct-a.json",
            "phala-direct-b.json",
            "near-ai-a.json",
            "tinfoil.json",
            "chutes-embed.json",
        ] {
            let document = fixture(name);
            let packed = pack(&document);
            let rebuilt = unpack_with_chunks(&packed);
            assert_eq!(rebuilt, document, "{name} must rebuild byte-exact");
            assert_eq!(
                digest::sha256_bare_hex(&rebuilt),
                digest::sha256_bare_hex(&document),
            );
        }
    }

    /// Real production documents must take the CAS path (not the Whole
    /// fallback) — if an upstream format change ever drives them to Whole,
    /// this test fails and the fallback still keeps serving correct bytes.
    #[test]
    fn real_documents_take_the_cas_path() {
        for name in ["phala-direct-a.json", "near-ai-a.json", "tinfoil.json"] {
            let document = fixture(name);
            match pack(&document) {
                Packed::Cas { .. } => {}
                Packed::Whole(_) => panic!("{name} unexpectedly fell back to Whole"),
            }
        }
    }

    /// Two consecutive re-verification rounds of one upstream share almost
    /// all chunks: the marginal write for the second document is only the
    /// fresh quote / GPU evidence / path nodes.
    #[test]
    fn consecutive_rounds_dedup() {
        let a = fixture("phala-direct-a.json");
        let b = fixture("phala-direct-b.json");
        let Packed::Cas { chunks: ca, .. } = pack(&a) else {
            panic!("a packs as CAS")
        };
        let Packed::Cas { chunks: cb, .. } = pack(&b) else {
            panic!("b packs as CAS")
        };
        let have: std::collections::HashSet<&String> = ca.iter().map(|(d, _)| d).collect();
        let marginal: usize = cb
            .iter()
            .filter(|(d, _)| !have.contains(d))
            .map(|(_, p)| p.len())
            .sum();
        // Measured on the production series: ~9.1KB of fresh chunks per round
        // (5KB quote binary + 4.1KB GPU evidence). Bound generously at 32KB.
        assert!(
            marginal < 32 * 1024,
            "marginal new chunk bytes {marginal} should be ~9KB"
        );
    }

    /// The internal duplicate (`all_attestations[0]` mirrors the top level in
    /// phala evidence) must not be stored twice within one document.
    #[test]
    fn internal_duplicate_is_stored_once() {
        let a = fixture("phala-direct-a.json");
        let Packed::Cas { chunks, skeleton } = pack(&a) else {
            panic!("packs as CAS")
        };
        let stored: usize = chunks.iter().map(|(_, p)| p.len()).sum::<usize>() + skeleton.len();
        // Raw document is 269KB; the evidence alone is 200KB with ~50%
        // internal duplication. Unique content is far smaller.
        assert!(
            stored < 180 * 1024,
            "unique content {stored} should be well below the raw size"
        );
    }

    /// Fallback: a document whose bytes cannot roundtrip through the rules
    /// (here: served bytes that are not the JCS form of the document, so the
    /// outer write cannot reproduce them) degrades to Whole and still serves
    /// the exact input.
    #[test]
    fn non_canonical_document_falls_back_to_whole() {
        // Pretty-printed (non-JCS) document: pack must not pretend CAS works.
        let document = b"{\n  \"a\": 1,\n  \"evidence\": {}\n}\n".to_vec();
        match pack(&document) {
            Packed::Whole(bytes) => assert_eq!(bytes, document),
            Packed::Cas { .. } => panic!("must fall back to Whole"),
        }
    }

    /// Embedded JSON with producer formatting we cannot reproduce (float
    /// formatting) keeps the string verbatim and still roundtrips.
    #[test]
    fn unreproducible_embedded_json_stays_verbatim() {
        let embedded = r#"{"ratio": 1.50, "note": "kept"}"#; // 1.50 is not ryu form
        let document = serde_json::json!({
            "api_version": "aci/1",
            "evidence": { "data": embedded }
        });
        let bytes = {
            let mut s = String::new();
            write_value(&document, true, false, &mut s);
            s.into_bytes()
        };
        let packed = pack(&bytes);
        assert_eq!(unpack_with_chunks(&packed), bytes);
    }

    /// A self-referencing chunk graph fails closed instead of recursing
    /// forever: the store's hash check makes such a file unusable, and the
    /// rebuild depth bound caps any residual recursion at parse level too.
    #[test]
    fn cyclic_reference_fails_closed() {
        let digest = "ab".repeat(32);
        let skeleton = format!(r#"{{"$r":"s:{digest}"}}"#);
        let mut cyclic = |_: &str| Some(skeleton.clone().into_bytes());
        assert!(unpack(skeleton.as_bytes(), false, &mut cyclic).is_none());
    }

    /// Rebuild recursion is depth-bounded: a payload chain nested deeper
    /// than MAX_REBUILD_DEPTH fails closed rather than overflowing the stack.
    #[test]
    fn rebuild_is_depth_bounded() {
        let mut chunks: Vec<(String, Vec<u8>)> = Vec::new();
        // Build a valid chain of subtree chunks, each referencing the next.
        let mut inner = Value::String("bottom".to_string());
        for _ in 0..(MAX_REBUILD_DEPTH + 8) {
            let canon = {
                let mut s = String::new();
                write_value(&inner, false, false, &mut s);
                s.into_bytes()
            };
            let digest = hex::encode(Sha256::digest(&canon));
            chunks.push((digest.clone(), canon));
            inner = Value::Object(serde_json::Map::from_iter([(
                "$r".to_string(),
                Value::String(format!("s:{digest}")),
            )]));
        }
        let mut skeleton = String::new();
        write_value(&inner, true, false, &mut skeleton);
        let mut by_digest = |d: &str| chunks.iter().find(|(k, _)| k == d).map(|(_, v)| v.clone());
        assert!(unpack(skeleton.as_bytes(), false, &mut by_digest).is_none());
    }

    /// unpack refuses a skeleton whose chunk is missing rather than serving
    /// wrong bytes.
    #[test]
    fn missing_chunk_is_corruption_not_a_miss() {
        let document = fixture("tinfoil.json");
        let Packed::Cas { skeleton, .. } = pack(&document) else {
            panic!("packs as CAS")
        };
        let mut no_chunks = |_: &str| -> Option<Vec<u8>> { None };
        assert!(unpack(&skeleton, false, &mut no_chunks).is_none());
    }

    /// The ASCII escaping writer matches the `ensure_ascii` flavor on the
    /// producer string we observed in production (`→` in near-ai app config).
    #[test]
    fn ascii_escaping_matches_observed_producer() {
        let mut out = String::new();
        write_string("a → b", true, &mut out);
        // The producer escapes non-ASCII as backslash-u sequences.
        let expected = "\"a \\u2192 b\"";
        assert_eq!(out, expected);
    }
}
