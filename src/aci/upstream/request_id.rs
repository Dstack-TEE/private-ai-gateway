//! The upstream's own identifier for one request.
//!
//! Read from a request-id response header, or from the `id` of a success body
//! when the upstream sent no such header. The value is upstream-controlled, so
//! it is kept only when it is a plain identifier: short, and built from a
//! character set that cannot carry prose.

use std::collections::HashMap;

use serde_json::Value;

const MAX_LEN: usize = 128;
const HEADER_SUFFIX: &str = "-request-id";

/// `value` if it is a plain identifier, otherwise nothing.
fn plain_identifier(value: &str) -> Option<String> {
    let plain = !value.is_empty()
        && value.len() <= MAX_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'));
    plain.then(|| value.to_string())
}

/// The request id an upstream stated in its response headers: `x-request-id`,
/// then `request-id`, then the first header in name order ending in
/// `-request-id`. Header names are expected in lowercase.
pub fn request_id_from_headers(headers: &HashMap<String, String>) -> Option<String> {
    let name = ["x-request-id", "request-id"]
        .into_iter()
        .find(|name| headers.contains_key(*name))
        .or_else(|| {
            headers
                .keys()
                .map(String::as_str)
                .filter(|name| name.ends_with(HEADER_SUFFIX))
                .min()
        })?;
    plain_identifier(headers.get(name)?)
}

/// The identifier a success body carries: a top-level `id`, or the `id` of the
/// `message` or `response` object a streamed opening event wraps it in.
pub fn request_id_from_body(body: &Value) -> Option<String> {
    [
        body.get("id"),
        body.pointer("/message/id"),
        body.pointer("/response/id"),
    ]
    .into_iter()
    .flatten()
    .find_map(Value::as_str)
    .and_then(plain_identifier)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn headers(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn header_precedence_is_fixed() {
        let all = headers(&[
            ("x-zeta-request-id", "zeta"),
            ("x-alpha-request-id", "alpha"),
            ("request-id", "bare"),
            ("x-request-id", "req_1"),
        ]);
        assert_eq!(request_id_from_headers(&all).as_deref(), Some("req_1"));
        let mut rest = all.clone();
        rest.remove("x-request-id");
        assert_eq!(request_id_from_headers(&rest).as_deref(), Some("bare"));
        rest.remove("request-id");
        assert_eq!(request_id_from_headers(&rest).as_deref(), Some("alpha"));
        assert_eq!(
            request_id_from_headers(&headers(&[("x-trace-id", "t")])),
            None
        );
    }

    #[test]
    fn only_plain_identifiers_are_kept() {
        for kept in [
            "req_45392d52ef9c",
            "chatcmpl-87e4",
            "a.b:c-d_e",
            &"a".repeat(128),
        ] {
            assert_eq!(
                request_id_from_headers(&headers(&[("x-request-id", kept)])).as_deref(),
                Some(kept)
            );
        }
        for dropped in ["", "two words", "a/b", "né", &"a".repeat(129)] {
            assert_eq!(
                request_id_from_headers(&headers(&[("x-request-id", dropped)])),
                None
            );
        }
    }

    #[test]
    fn body_id_is_read_from_each_success_shape() {
        assert_eq!(
            request_id_from_body(&json!({"id": "chatcmpl-1"})).as_deref(),
            Some("chatcmpl-1")
        );
        assert_eq!(
            request_id_from_body(&json!({"type": "message_start", "message": {"id": "msg_1"}}))
                .as_deref(),
            Some("msg_1")
        );
        assert_eq!(
            request_id_from_body(
                &json!({"type": "response.created", "response": {"id": "resp_1"}})
            )
            .as_deref(),
            Some("resp_1")
        );
        assert_eq!(request_id_from_body(&json!({"id": "not an id"})), None);
        assert_eq!(request_id_from_body(&json!({"id": 7})), None);
        assert_eq!(
            request_id_from_body(&json!({"error": {"message": "x"}})),
            None
        );
    }
}
