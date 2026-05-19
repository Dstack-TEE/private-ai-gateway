//! Canonical-JSON tests for the ACI value subset.

use private_ai_gateway::aci::canonical::{
    canonicalize, jcs_sha256_hex, sha256_hex, CanonicalError,
};
use serde_json::{json, Number, Value};

#[test]
fn sorts_object_keys_lexicographically_for_ascii() {
    let v = json!({ "b": 1, "a": 2 });
    assert_eq!(canonicalize(&v).unwrap(), br#"{"a":2,"b":1}"#);
}

#[test]
fn strips_whitespace() {
    let v = json!({ "a": [1, 2, 3] });
    assert_eq!(canonicalize(&v).unwrap(), br#"{"a":[1,2,3]}"#);
}

#[test]
fn preserves_array_order() {
    let v = json!([3, 1, 2]);
    assert_eq!(canonicalize(&v).unwrap(), b"[3,1,2]");
}

#[test]
fn emits_null_literal() {
    let v = json!({ "nonce": Value::Null });
    assert_eq!(canonicalize(&v).unwrap(), br#"{"nonce":null}"#);
}

#[test]
fn distinguishes_null_from_string_null() {
    let a = canonicalize(&json!({ "nonce": Value::Null })).unwrap();
    let b = canonicalize(&json!({ "nonce": "null" })).unwrap();
    assert_ne!(a, b);
    assert_eq!(b, br#"{"nonce":"null"}"#);
}

#[test]
fn integer_canonical_form() {
    assert_eq!(canonicalize(&json!({ "v": 0i64 })).unwrap(), br#"{"v":0}"#);
    assert_eq!(
        canonicalize(&json!({ "v": -1i64 })).unwrap(),
        br#"{"v":-1}"#
    );
    assert_eq!(
        canonicalize(&json!({ "v": 12345i64 })).unwrap(),
        br#"{"v":12345}"#
    );
}

#[test]
fn rejects_float() {
    // 1.5 cannot be expressed as a JSON integer; the value carries a
    // fractional part and serde_json produces a `f64`-shaped Number.
    let v = Value::Object({
        let mut m = serde_json::Map::new();
        m.insert(
            "x".to_string(),
            Value::Number(Number::from_f64(1.5).unwrap()),
        );
        m
    });
    let err = canonicalize(&v).unwrap_err();
    assert!(matches!(err, CanonicalError::FloatNotAllowed));
}

#[test]
fn nested_objects_sorted_at_each_level() {
    let v = json!({ "outer": { "b": 1, "a": [{"y": 2, "x": 1}] } });
    let expected: &[u8] = br#"{"outer":{"a":[{"x":1,"y":2}],"b":1}}"#;
    assert_eq!(canonicalize(&v).unwrap(), expected);
}

#[test]
fn sha256_hex_format() {
    let s = sha256_hex(b"abc");
    assert!(s.starts_with("sha256:"));
    assert_eq!(
        s,
        "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn jcs_sha256_is_stable_under_key_reorder() {
    let a = jcs_sha256_hex(&json!({ "x": 1, "y": 2 })).unwrap();
    let b = jcs_sha256_hex(&json!({ "y": 2, "x": 1 })).unwrap();
    assert_eq!(a, b);
}

#[test]
fn unicode_strings_pass_through_in_utf8() {
    let v = json!({ "s": "héllo" });
    assert_eq!(canonicalize(&v).unwrap(), b"{\"s\":\"h\xc3\xa9llo\"}");
}

#[test]
fn control_characters_use_short_escapes_when_available() {
    let v = json!({ "s": "a\nb\tc" });
    assert_eq!(canonicalize(&v).unwrap(), br#"{"s":"a\nb\tc"}"#);
}

#[test]
fn control_characters_otherwise_use_unicode_escape() {
    // \u{0001} has no short escape; \u{0007} (BEL) has no short
    // escape. Both must serialise as \u00XX.
    let mut s = String::new();
    s.push('\u{0001}');
    s.push('\u{0007}');
    let v = json!({ "s": s });
    let bytes = canonicalize(&v).unwrap();
    let mut expected: Vec<u8> = Vec::new();
    expected.extend_from_slice(b"{\"s\":\"");
    expected.extend_from_slice(b"\\u0001");
    expected.extend_from_slice(b"\\u0007");
    expected.extend_from_slice(b"\"}");
    assert_eq!(bytes, expected);
}

#[test]
fn keys_sort_by_utf16_code_units_not_codepoints_for_supplementary_plane() {
    // U+10000 GOTHIC LETTER AHSA encodes in UTF-16 as 0xD800 0xDC00,
    // both surrogate units. U+E000 PRIVATE USE encodes as a single
    // BMP unit 0xE000. By code-point order, U+10000 > U+E000, but by
    // UTF-16 code-unit order the surrogate (0xD800) < 0xE000, so
    // U+10000 sorts FIRST under JCS.
    let smp_key = "\u{10000}".to_string();
    let bmp_key = "\u{E000}".to_string();

    let mut map = serde_json::Map::new();
    map.insert(bmp_key.clone(), Value::from(2));
    map.insert(smp_key.clone(), Value::from(1));
    let v = Value::Object(map);

    let bytes = canonicalize(&v).unwrap();
    // The supplementary-plane key (U+10000 → UTF-8 F0 90 80 80) must
    // come before the BMP key (U+E000 → UTF-8 EE 80 80).
    let smp_pos = bytes
        .windows(4)
        .position(|w| w == b"\xF0\x90\x80\x80")
        .expect("SMP key should appear in canonical bytes");
    let bmp_pos = bytes
        .windows(3)
        .position(|w| w == b"\xEE\x80\x80")
        .expect("BMP key should appear in canonical bytes");
    assert!(
        smp_pos < bmp_pos,
        "UTF-16 sort must place surrogate-encoded SMP key before U+E000"
    );
}
