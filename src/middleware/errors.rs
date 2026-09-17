//! Client-facing error responses, shaped per downstream API surface.
//!
//! Two surfaces are served: an OpenAI-compatible surface (chat/completions,
//! completions, embeddings, responses) and an Anthropic-compatible surface
//! (messages). Success responses are converted per surface elsewhere; these
//! builders do the same for errors so each SDK gets a parseable envelope.
//!
//! Upstream error detail is never passed through raw: status, body, and headers
//! are always rebuilt here so provider internals cannot leak.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::{
    http::{header::CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{json, Value};

// Re-exported so call sites keep importing client-facing error shapes from one
// place; the definitions are shared because the service layer builds them too.
pub use crate::error_payload::{error_type, upstream_message, Surface};
pub use crate::sse_protocol::{
    chat_gateway_error, responses_error_event, responses_gateway_code, sse_protocol,
    stream_error_event, stream_error_tail, SseProtocol,
};

use super::types::ErrorClass;
use crate::error_payload::envelope;

/// Flatten an upstream status to the client-facing status. The mapping is uniform
/// across surfaces; only the envelope and `error.type` are surface-aware.
pub fn map_upstream_status(status: u16) -> u16 {
    match status {
        // 413 sits with the other request-fault statuses: an oversized request
        // is the caller's to fix, and flattening it to 502 would both invite a
        // retry that cannot succeed and contradict the relay path above, which
        // returns it unflattened whenever the provider's message survives
        // scrubbing.
        400 | 404 | 413 | 422 => status,
        429 => 429,
        503 => 503,
        504 => 504,
        _ => 502,
    }
}

/// 4xx other than auth/billing/rate-limit (401/402/403/429) describe a problem
/// with the caller's own request, so the provider's message is worth surfacing
/// (always re-wrapped in our envelope, never the raw upstream response).
pub fn is_actionable_client_error(status: u16) -> bool {
    (400..500).contains(&status) && !matches!(status, 401..=403 | 429)
}

/// Serialize the surface error envelope to bytes (for the E2EE generated path).
pub fn envelope_bytes(
    surface: Surface,
    error_type: &str,
    message: &str,
    request_id: Option<&str>,
) -> Vec<u8> {
    serde_json::to_vec(&envelope(surface, error_type, message, request_id)).unwrap_or_default()
}

fn rate_limit_envelope(surface: Surface, message: &str, request_id: Option<&str>) -> Value {
    let mut body = envelope(surface, "rate_limit_error", message, request_id);
    // OpenAI clients expect a string error code on rate limits.
    if surface == Surface::Openai {
        body["error"]["code"] = json!("rate_limit_exceeded");
    }
    body
}

/// Serialize the rate-limit envelope to bytes (for the E2EE generated path).
pub fn rate_limit_envelope_bytes(
    surface: Surface,
    message: &str,
    request_id: Option<&str>,
) -> Vec<u8> {
    serde_json::to_vec(&rate_limit_envelope(surface, message, request_id)).unwrap_or_default()
}

/// The standard rate-limit response headers (`X-RateLimit-*`, `Retry-After`).
pub fn rate_limit_headers(limit: i64, reset_at: i64) -> Vec<(&'static str, String)> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let retry_after = (reset_at - now).max(1);
    vec![
        ("X-RateLimit-Limit", limit.to_string()),
        ("X-RateLimit-Remaining", "0".to_string()),
        ("X-RateLimit-Reset", reset_at.to_string()),
        ("Retry-After", retry_after.to_string()),
    ]
}

fn json_response(body: &Value, status: u16, extra_headers: &[(&str, String)]) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    for (name, value) in extra_headers {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }
    (
        status,
        headers,
        serde_json::to_vec(body).unwrap_or_default(),
    )
        .into_response()
}

/// Build a client-facing error response in the right envelope for `surface`.
pub fn error_response(
    surface: Surface,
    status: u16,
    error_type: &str,
    message: &str,
    request_id: Option<&str>,
) -> Response {
    json_response(
        &envelope(surface, error_type, message, request_id),
        status,
        &[],
    )
}

/// A 429 response carrying the standard rate-limit headers.
pub fn rate_limit_response(
    surface: Surface,
    message: &str,
    limit: i64,
    reset_at: i64,
    request_id: Option<&str>,
) -> Response {
    json_response(
        &rate_limit_envelope(surface, message, request_id),
        429,
        &rate_limit_headers(limit, reset_at),
    )
}

/// The upstream's message, verbatim. Callers that classify against it (the
/// image-URL matcher compares it to the *client's own* URL) need it unscrubbed;
/// callers that relay it to the client must use `client_safe_error_message`.
fn extract_error_message(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    match value.get("error") {
        Some(Value::String(message)) => Some(message.clone()),
        Some(error) => error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        // Some providers put the message at the top level
        // (`{"code":400,"message":"..."}` or `{"message":"...","type":"..."}`)
        // with no `error` member at all.
        None => value
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// The class an upstream error response is recorded under.
///
/// The body is read only to choose among fixed classes; none of it is carried
/// into the result. The account signal honours the provider-declared kind alone
/// (see [`declares_quota_exhausted`]), so a caller cannot steer the record by
/// putting a provider's wording into a prompt.
pub(crate) fn classify_upstream(
    received_body: &[u8],
    upstream_status: u16,
    upstream_body: &[u8],
) -> ErrorClass {
    if declares_quota_exhausted(upstream_status, upstream_body) {
        ErrorClass::UpstreamQuotaExhausted
    } else if is_upstream_capacity_signal(upstream_status, upstream_body) {
        ErrorClass::UpstreamCapacity
    } else if classify_image_input_error(received_body, upstream_status, upstream_body).is_some() {
        ErrorClass::UpstreamImageFetchFailed
    } else {
        ErrorClass::UpstreamHttpError
    }
}

/// The upstream's message, fit to hand to the client, or `None` to fall back to
/// our own per-status text. It goes into the response to the caller and nowhere
/// else.
pub(crate) fn client_safe_error_message(body: &[u8]) -> Option<String> {
    scrub_identifying_markers(&extract_error_message(body)?)
}

/// A single error-message string made safe to relay: scrubbed by the same shape
/// rules as the buffered path, or replaced with a generic line when nothing safe
/// remains. Used for an in-band error on a streaming chunk, where there is no
/// per-status text to fall back to and the message must stay non-empty.
pub(crate) fn client_safe_error_text(message: &str) -> String {
    scrub_identifying_markers(message)
        .unwrap_or_else(|| "the provider returned an error".to_string())
}

/// An upstream error message, fit to relay — or `None` to fall back to our own
/// per-status text.
///
/// A message that carries a URL or a hostname is dropped entirely rather than
/// cleaned further. Everything else is relayed as written, a provider's error
/// code prefix included: a dotted head is far more often the field path the
/// caller needs (`messages.0.content: …`) than a code namespace.
///
/// Matched by shape, never by a maintained list of names. A bare company name
/// or a vendor's code namespace in otherwise-neutral prose is therefore relayed
/// — an accepted residual: neither names a host, and a name list would have to
/// be updated for every new upstream.
///
/// Biased toward dropping when a structural marker is present: a message we
/// withhold costs the caller some detail, a message with a host in it can point
/// at who served the request.
fn scrub_identifying_markers(raw: &str) -> Option<String> {
    let message = raw.trim();
    if message.is_empty() || looks_identifying(message) {
        return None;
    }
    Some(message.to_string())
}

/// Whether a message carries a URL or a host — the structural shapes that point
/// at who served a request. Names are matched by shape only; there is
/// deliberately no list of company names to keep current. A bare request or
/// trace id is not one of these shapes: it names no one, while the callers'
/// own hashes and keys have that shape, so treating it as identifying threw
/// away messages the caller could act on.
/// `pub(crate)`: `response_transform` applies the same check to an error's
/// `param` value, where the fallback is null rather than a generic line.
pub(crate) fn looks_identifying(message: &str) -> bool {
    if message.contains("://") {
        return true;
    }
    message
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_'))
        // `.`/`-`/`_` are kept inside a token so a host stays whole, but they are
        // also sentence punctuation: a host ending a sentence (`… api.acme.ai.`)
        // would otherwise carry a trailing `.`, leaving an empty final label that
        // fails the shape check below. A real host or IP never starts or ends
        // with one, so trimming them is safe and closes that leak.
        .map(|word| word.trim_matches(|c| c == '.' || c == '-' || c == '_'))
        .any(is_hostname_like)
}

/// Top-level domains a provider's infrastructure realistically appears under.
/// Deliberately a closed list rather than "any short alphabetic suffix": a
/// dotted token is far more often a field path (`temperature.value`), a library
/// (`Node.js`) or a version than a host, and treating those as identifying threw
/// away messages the caller could have acted on. TLDs that double as common
/// field-path or filename segments (`app`, `run`, `dev`, `cloud`, `info`, `sh`, …)
/// are deliberately excluded for the same reason — `config.dev`/`app.run`/
/// `entrypoint.sh` are not hosts. A host under some TLD outside this list survives
/// — that is the accepted cost of not discarding good errors, and `://` still
/// catches the common shape.
const HOST_TLDS: &[&str] = &[
    "com", "net", "org", "io", "ai", "cn", "eu", "uk", "de", "fr", "jp", "gg", "xyz", "local",
    "internal",
];

/// `api.acme.ai`, `10.0.0.7` — a dotted token under a known TLD, or a dotted
/// quad of octets.
fn is_hostname_like(word: &str) -> bool {
    let parts: Vec<&str> = word.split('.').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    // An IPv4 literal. A four-part version string is indistinguishable and is
    // dropped with it; three-part versions (the common shape) are not, because
    // they fail both this and the TLD test below.
    if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
        return true;
    }
    let last = parts[parts.len() - 1].to_ascii_lowercase();
    HOST_TLDS.contains(&last.as_str())
        && parts[..parts.len() - 1]
            .iter()
            .all(|p| p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// The host component of an `http(s)://` URL (`https://a.example/x?y` -> `a.example`).
/// Used to correlate an upstream error message with a request image URL when the
/// message names only the host (e.g. a DNS failure) rather than the full URL.
fn url_host(url: &str) -> Option<&str> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let end = rest.find(['/', ':', '?', '#']).unwrap_or(rest.len());
    let host = &rest[..end];
    (!host.is_empty()).then_some(host)
}

/// Whether `message` names `host` as a standalone host token. Two guards keep this
/// from misfiring on an unrelated provider error: the host must be domain-like (have
/// a dot), so a bare single-label host such as `internal` can't match the word
/// "internal" in a generic message; and the token must match whole (splitting the
/// message on non-host characters), so `a.co` doesn't match inside `banana.com`.
fn message_references_host(message: &str, host: &str) -> bool {
    host.contains('.')
        && message
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '-'))
            .any(|token| token == host)
}

/// The fetchable remote URL of a single content part, across the request shapes this
/// gateway serves on one code path: OpenAI chat `{"type":"image_url","image_url":{"url"}}`,
/// Responses `{"type":"input_image","image_url":"<url>"}` (image_url may be a bare
/// string or an object), and Anthropic `{"type":"image","source":{"type":"url","url"}}`.
/// Data-URI (`data:`) sources carry no fetchable URL and yield `None`.
fn image_part_url(part: &Value) -> Option<&str> {
    match part.get("type").and_then(Value::as_str) {
        Some("image_url") | Some("input_image") => {
            let image_url = part.get("image_url")?;
            image_url
                .as_str()
                .or_else(|| image_url.get("url").and_then(Value::as_str))
        }
        Some("image") => part
            .get("source")
            .filter(|source| source.get("type").and_then(Value::as_str) == Some("url"))
            .and_then(|source| source.get("url"))
            .and_then(Value::as_str),
        _ => None,
    }
}

/// Collect the remote (`http`/`https`) image URLs a request asks the upstream to
/// fetch. Covers every surface served by the completion path — OpenAI chat and
/// Anthropic messages (`messages[].content[]`) and Responses (`input[]`, whose image
/// parts may sit directly in the array or nested under `content`). A cheap substring
/// guard skips the JSON parse entirely when the body has no image content at all.
fn remote_image_urls(request_body: &[u8]) -> Vec<String> {
    let Ok(text) = std::str::from_utf8(request_body) else {
        return Vec::new();
    };
    if !text.contains("image") {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return Vec::new();
    };
    let mut urls = Vec::new();
    for key in ["messages", "input"] {
        let Some(items) = value.get(key).and_then(Value::as_array) else {
            continue;
        };
        for item in items {
            // Chat/messages nest image parts under `content`; Responses may also
            // place a part directly in the `input` array, so check the item itself.
            let nested = item.get("content").and_then(Value::as_array);
            for part in std::iter::once(item).chain(nested.into_iter().flatten()) {
                if let Some(url) = image_part_url(part) {
                    if url.starts_with("https://") || url.starts_with("http://") {
                        urls.push(url.to_string());
                    }
                }
            }
        }
    }
    urls
}

/// When a non-2xx upstream error was caused by a remote image URL in the client's
/// request that the upstream could not fetch, return a normalized, client-facing
/// message and let the caller treat it as a 400. Detection is URL-correlation
/// based: the request carried a remote image URL AND the upstream error message
/// names that URL (or its host). This keeps false positives out — an unrelated 5xx
/// never matches. Returns `None` when it is not an image-input error (caller keeps
/// the existing mapping). The status check runs first, so success responses never
/// touch the request body.
pub fn classify_image_input_error(
    received_body: &[u8],
    upstream_status: u16,
    upstream_body: &[u8],
) -> Option<String> {
    if (200..300).contains(&upstream_status) {
        return None;
    }
    let message = extract_error_message(upstream_body)?;
    let matched = remote_image_urls(received_body).into_iter().find(|url| {
        message.contains(url.as_str())
            || url_host(url).is_some_and(|host| message_references_host(&message, host))
    })?;
    Some(format!(
        "Failed to fetch the image at the provided URL: {matched}. \
         Ensure the URL is correct and publicly accessible."
    ))
}

/// The error-envelope surface for an endpoint path: `/v1/messages` is
/// Anthropic-shaped, everything else OpenAI-shaped.
pub fn surface_for_path(endpoint_path: &str) -> Surface {
    if endpoint_path == crate::aggregator::service::MESSAGES_PATH {
        Surface::Anthropic
    } else {
        Surface::Openai
    }
}

/// The client-facing `(status, envelope bytes)` for an image-input error, or
/// `None` when the upstream error is not one (see
/// [`classify_image_input_error`]). The single place the 400 response for this
/// error is assembled — every serving path applies these parts as-is.
pub fn image_input_error_parts(
    surface: Surface,
    received_body: &[u8],
    upstream_status: u16,
    upstream_body: &[u8],
    request_id: Option<&str>,
) -> Option<(u16, Vec<u8>)> {
    let message = classify_image_input_error(received_body, upstream_status, upstream_body)?;
    Some((
        400,
        envelope_bytes(surface, error_type(surface, 400), &message, request_id),
    ))
}

/// Marker substring an upstream uses to signal it has no available capacity or
/// targets to serve the request. Matched raw against the error body because the
/// message can sit outside the usual `error`/`error.message` fields.
const UPSTREAM_CAPACITY_MARKER: &[u8] = b"exhausted all available targets";

/// The client-facing `(status, envelope bytes)` for an upstream capacity signal,
/// or `None` when the upstream error is not one. An upstream reporting it has no
/// available capacity/targets is busy, not faulty: surface it as `429`
/// (rate-limited) rather than a `5xx` outage, so it is treated as load, not an
/// error. Peer to [`image_input_error_parts`] — both remap a recognized upstream
/// error body to a specific client status.
/// Whether an upstream outcome is a capacity signal: a literal 429, or the
/// recognized 5xx capacity/no-targets body that error normalization also
/// surfaces to clients as 429. The single classification shared by error
/// normalization and the forwarder's capacity-retry targeting — the two must
/// never disagree about what "capacity" means, or a request could be told
/// 429 without ever having been eligible for the retry that 429s get.
pub(crate) fn is_upstream_capacity_signal(status: u16, body: &[u8]) -> bool {
    if status == 429 {
        return true;
    }
    (500..600).contains(&status)
        && body
            .windows(UPSTREAM_CAPACITY_MARKER.len())
            .any(|w| w == UPSTREAM_CAPACITY_MARKER)
}

/// Field values and prose a provider uses to say *this gateway's* account with
/// it is out of quota or credit. Kept small and extended from what upstreams
/// are actually seen to send; failing to recognize one only leaves today's
/// behaviour, so the list is safe to grow lazily.
const QUOTA_EXHAUSTED_KINDS: &[&str] = &[
    "insufficient_quota",
    "credit_balance_exhausted",
    "billing_error",
    "billing_not_active",
];
/// Prose markers for providers that report this only in the message, under a
/// kind they also use for ordinary request faults. Deliberately narrow: an
/// upstream 4xx often quotes the caller's own input back, so a marker loose
/// enough to appear in a prompt would let a caller drive this classification.
/// Each of these is a whole provider-authored clause, not a word that could
/// ride in quoted content.
const QUOTA_EXHAUSTED_PROSE: &[&str] = &[
    "credit balance is too low",
    "exceeded your current quota",
    "no credits remaining",
];

/// Whether an upstream refusal says the provider is unpaid rather than busy.
///
/// The distinction exists only in the response body — the same 429 carries both
/// "you are going too fast" and "your account is out of credit", and a provider
/// may report the latter under 400 instead. Nothing downstream can recover it:
/// the body stops here, and what reaches the usage record is this gateway's own
/// generic text. So it is classified here or not at all.
pub(crate) fn is_quota_exhausted_signal(status: u16, body: &[u8]) -> bool {
    if !(400..500).contains(&status) {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    value
        .get("error")
        .unwrap_or(&value)
        .as_object()
        .is_some_and(is_quota_exhausted_error)
}

/// The same classification against an error object already in hand — the shape
/// an in-band error takes inside a 200 stream, where there is no status to
/// consult. Both paths must agree: a provider that reports this condition
/// under a kind the relay allowlist recognizes (some do, reusing the same
/// `invalid_request_error` they use for real request faults) would otherwise
/// be suppressed on one path and relayed verbatim on the other.
pub(crate) fn is_quota_exhausted_error(error: &serde_json::Map<String, Value>) -> bool {
    declared_kind_is_quota_exhausted(error)
        || error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_ascii_lowercase)
            .is_some_and(|message| {
                QUOTA_EXHAUSTED_PROSE
                    .iter()
                    .any(|marker| message.contains(marker))
            })
}

/// The account signal as the provider *stated* it, in a kind field it chose —
/// no prose. Only this may drive anything acted on beyond the response itself.
///
/// An upstream 4xx often quotes the caller's own input back, so a prompt can
/// put a prose marker into `error.message`. Withholding a message on that
/// basis costs a caller some detail; letting it reclassify the attempt would
/// hand a caller a lever over how this route is judged, which no request
/// should have.
pub(crate) fn declares_quota_exhausted(status: u16, body: &[u8]) -> bool {
    if !(400..500).contains(&status) {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return false;
    };
    value
        .get("error")
        .unwrap_or(&value)
        .as_object()
        .is_some_and(declared_kind_is_quota_exhausted)
}

/// The status an upstream attempt is recorded under.
///
/// A provider refusing because our account with it is unpaid records as 402
/// whatever status it chose to say that under, so the row names the condition
/// rather than the wording. Every recorded attempt goes through this, not just
/// the one whose answer was committed: an unpaid route is usually *not* the
/// committed one — the walk fails over past it — and recording that attempt
/// under its raw status would leave the condition invisible exactly where it
/// matters most, on a model with more than one route.
pub(crate) fn recorded_attempt_status(status: u16, body: &[u8]) -> u16 {
    if declares_quota_exhausted(status, body) {
        402
    } else {
        status
    }
}

fn declared_kind_is_quota_exhausted(error: &serde_json::Map<String, Value>) -> bool {
    ["code", "type"].into_iter().any(|key| {
        error
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|kind| QUOTA_EXHAUSTED_KINDS.contains(&kind))
    })
}

/// The client-facing parts for an upstream that refused because it is unpaid.
///
/// Two things have to happen here, and neither can happen anywhere else. The
/// client must not be told to retry: a 429's "retry after some time" promises a
/// recovery that will not come, and relaying the provider's own wording (which
/// a 4xx otherwise earns) would tell the caller *their* balance is empty when
/// it is this gateway's. So it takes the status an upstream 402 already takes —
/// 502 — with the generic unavailable line. The usage record then follows the
/// same reclassification, which is what moves it out of the neutral rate-limit
/// bucket it would otherwise sit in.
fn quota_exhausted_error_parts(
    surface: Surface,
    upstream_status: u16,
    upstream_body: &[u8],
    request_id: Option<&str>,
) -> Option<(u16, Vec<u8>)> {
    if !is_quota_exhausted_signal(upstream_status, upstream_body) {
        return None;
    }
    let status = map_upstream_status(402);
    Some((
        status,
        envelope_bytes(
            surface,
            error_type(surface, status),
            upstream_message(402),
            request_id,
        ),
    ))
}

fn capacity_error_parts(
    surface: Surface,
    upstream_status: u16,
    upstream_body: &[u8],
    request_id: Option<&str>,
) -> Option<(u16, Vec<u8>)> {
    if !(500..600).contains(&upstream_status) {
        return None;
    }
    if !is_upstream_capacity_signal(upstream_status, upstream_body) {
        return None;
    }
    Some((
        429,
        envelope_bytes(
            surface,
            error_type(surface, 429),
            upstream_message(429),
            request_id,
        ),
    ))
}

/// Normalize a non-2xx upstream response into the client-facing status and the
/// surface-shaped error body bytes. For actionable client errors the provider's
/// own message is re-wrapped at the original status; everything else gets a
/// generic sanitized message at the mapped status.
pub fn normalize_upstream_error_parts(
    surface: Surface,
    upstream_status: u16,
    body: &[u8],
    received_body: &[u8],
    request_id: Option<&str>,
) -> (u16, Vec<u8>) {
    // A failed fetch of a client-supplied image URL is the caller's problem, not a
    // provider fault: surface it as a 400 with a message naming the URL.
    if let Some(parts) =
        image_input_error_parts(surface, received_body, upstream_status, body, request_id)
    {
        return parts;
    }
    // A provider refusing because it is unpaid is not rate-limiting us, and the
    // refusal is not the caller's to act on. Checked before both the capacity
    // remap and the actionable-4xx relay, which would otherwise hand the caller
    // a retry promise or the provider's own account wording.
    if let Some(parts) = quota_exhausted_error_parts(surface, upstream_status, body, request_id) {
        return parts;
    }
    // A provider signalling it is out of capacity is busy, not broken: 429, not 5xx.
    if let Some(parts) = capacity_error_parts(surface, upstream_status, body, request_id) {
        return parts;
    }
    if is_actionable_client_error(upstream_status) {
        if let Some(message) = client_safe_error_message(body) {
            return (
                upstream_status,
                envelope_bytes(
                    surface,
                    error_type(surface, upstream_status),
                    &message,
                    request_id,
                ),
            );
        }
    }
    let status = map_upstream_status(upstream_status);
    (
        status,
        envelope_bytes(
            surface,
            error_type(surface, status),
            upstream_message(upstream_status),
            request_id,
        ),
    )
}

/// Wrap `(status, envelope bytes)` parts into a JSON error response.
pub fn parts_response(status: u16, body: Vec<u8>) -> Response {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    (status, headers, body).into_response()
}

/// Normalize a non-2xx upstream response into a surface-shaped error response.
pub fn normalize_upstream_error(
    surface: Surface,
    upstream_status: u16,
    body: &[u8],
    received_body: &[u8],
    request_id: Option<&str>,
) -> Response {
    let (status, bytes) =
        normalize_upstream_error_parts(surface, upstream_status, body, received_body, request_id);
    parts_response(status, bytes)
}

#[cfg(test)]
mod scrub_tests {
    use super::scrub_identifying_markers;

    /// The head before a colon is the caller's to read: it is usually the field
    /// path at fault, and a provider's code namespace in the same position is
    /// relayed with it rather than guessed apart.
    #[test]
    fn keeps_the_head_before_a_colon() {
        for message in [
            "messages.0.content: Input should be a valid string",
            "body.tools.0.function.parameters: invalid JSON schema",
            "<400> Module.Sub.BadParam: The combination is not supported by this model",
        ] {
            assert_eq!(scrub_identifying_markers(message).as_deref(), Some(message));
        }
    }

    /// Plain prose and a non-ASCII body are relayed verbatim, not garbled.
    #[test]
    fn keeps_a_clean_message_verbatim() {
        for message in [
            "warning: value out of range",
            "temperature is invalid \u{2014} must be between 0 and 1",
            "The model does not exist or you do not have access to it.",
            "Expected temperature to be at most 2, received 99",
            "'temperature' must be Float",
        ] {
            assert_eq!(scrub_identifying_markers(message).as_deref(), Some(message));
        }
    }

    /// A URL, a hostname or an IP points at who served the request, and is
    /// dropped by shape — no list of names involved.
    #[test]
    fn drops_a_message_that_still_identifies_the_upstream() {
        for message in [
            "upstream https://api.acme.ai/v1 returned 500",
            "no route to inference-7.internal.acme.io",
            "backend 10.0.0.7 refused the connection",
        ] {
            assert_eq!(
                scrub_identifying_markers(message),
                None,
                "leaked: {message}"
            );
        }
    }

    /// A request or trace id names no one, and the callers' own hashes and keys
    /// have the same shape, so an id is not a reason to discard an actionable
    /// message.
    #[test]
    fn relays_a_message_carrying_only_an_id() {
        for message in [
            "request 136e8d8a-e1ee-94c3-90f7-3b3bd3ecbf29 failed",
            "trace 4f9a2c1de88b0771ab34 not found",
        ] {
            assert_eq!(
                scrub_identifying_markers(message).as_deref(),
                Some(message),
                "wrongly dropped: {message}"
            );
        }
    }

    /// Accepted residual of dropping the name list: a bare company name in
    /// otherwise-neutral prose is relayed. Documented as a test so the choice is
    /// explicit — this shape has not been seen in practice, and the
    /// shapes that were are caught above.
    #[test]
    fn relays_a_bare_vendor_name_in_neutral_prose() {
        assert_eq!(
            scrub_identifying_markers("acmecloud rejected the request").as_deref(),
            Some("acmecloud rejected the request")
        );
    }

    /// A dotted token is usually a field path or a library, not a host. Treating
    /// every one as identifying discarded messages the caller could act on, which
    /// is the opposite of useful: these are the most actionable errors there are.
    #[test]
    fn keeps_messages_whose_dotted_tokens_are_not_hosts() {
        for message in [
            "Invalid 'temperature.value': must be a number",
            "Node.js runtime error",
            "invalid request.body",
            "responseFormat.type is not supported",
            "response_format.type is not supported",
            "expected schema.properties.name to be a string",
            "upgrade to version 1.2.3",
            "temperature must be between 0.0 and 2.0",
        ] {
            assert_eq!(
                scrub_identifying_markers(message).as_deref(),
                Some(message),
                "wrongly dropped: {message}"
            );
        }
    }

    /// Narrowing the host rule to known TLDs must not let a real host through.
    #[test]
    fn still_drops_real_hosts_and_addresses() {
        for message in [
            "cannot connect to host files.example.com:443",
            "resolve failed for api.acme.ai",
            // A host or IP that ends the sentence carries a trailing period.
            "could not resolve host api.acme.ai.",
            "connection refused by 10.0.0.7.",
        ] {
            assert_eq!(
                scrub_identifying_markers(message),
                None,
                "leaked: {message}"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_upstream_capacity_signal;

    #[test]
    fn top_level_message_bodies_still_yield_a_client_message() {
        // Provider error bodies without an `error` member, message at the top.
        for body in [
            br#"{"code":400, "reason":"INVALID_REQUEST_BODY", "message":"max_tokens must be between 0 and 393216"}"#.as_slice(),
            br#"{"message":"invalid request error","type":"invalid_request_error"}"#.as_slice(),
        ] {
            let got =
                super::client_safe_error_message(body).expect("message extracted");
            assert!(!got.is_empty());
        }
        // No message anywhere stays None rather than fabricating one.
        assert!(super::client_safe_error_message(br#"{"code":400}"#).is_none());
    }

    #[test]
    fn upstream_errors_are_recorded_under_a_fixed_class() {
        use super::{classify_upstream, ErrorClass};
        let image_request =
            br#"{"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://img.example/cat.png"}}]}]}"#;
        let cases: [(&[u8], u16, &[u8], ErrorClass); 5] = [
            (
                b"{}",
                400,
                br#"{"error":{"message":"bad field","type":"invalid_request_error"}}"#,
                ErrorClass::UpstreamHttpError,
            ),
            (
                b"{}",
                429,
                br#"{"error":{"code":"insufficient_quota","message":"out of credit"}}"#,
                ErrorClass::UpstreamQuotaExhausted,
            ),
            (
                b"{}",
                429,
                br#"{"error":{"message":"slow down"}}"#,
                ErrorClass::UpstreamCapacity,
            ),
            (
                image_request,
                500,
                br#"{"error":{"message":"could not fetch https://img.example/cat.png"}}"#,
                ErrorClass::UpstreamImageFetchFailed,
            ),
            (b"{}", 500, b"not json", ErrorClass::UpstreamHttpError),
        ];
        for (request, status, body, expected) in cases {
            assert_eq!(classify_upstream(request, status, body), expected);
        }
    }

    /// The same 429 carries both "slow down" and "your account is unpaid", and
    /// some providers report the latter under 400. Only the unpaid one is
    /// reclassified — to the status an upstream 402 already takes, with our own
    /// wording, so the caller is neither promised a retry nor told their own
    /// balance is empty.
    #[test]
    fn an_unpaid_provider_is_reclassified_away_from_rate_limiting() {
        let unpaid = [
            (429, br#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota, please check your plan and billing details."}}"#.to_vec()),
            (400, br#"{"error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the API."}}"#.to_vec()),
        ];
        for (status, body) in unpaid {
            let (mapped, envelope) =
                normalize_upstream_error_parts(Surface::Openai, status, &body, b"{}", None);
            assert_eq!(mapped, 502, "status {status}");
            let value: Value = serde_json::from_slice(&envelope).unwrap();
            assert_eq!(value["error"]["type"], "upstream_error");
            assert_eq!(
                value["error"]["message"],
                "The upstream provider is currently unavailable"
            );
            let wire = String::from_utf8(envelope).unwrap();
            assert!(!wire.contains("quota"), "leaked account wording: {wire}");
            assert!(!wire.contains("credit"), "leaked account wording: {wire}");
        }

        // A plain rate limit is untouched: still a 429, still retryable.
        let (mapped, envelope) = normalize_upstream_error_parts(
            Surface::Openai,
            429,
            br#"{"error":{"code":"rate_limit_exceeded","message":"Too many requests"}}"#,
            b"{}",
            None,
        );
        assert_eq!(mapped, 429);
        let value: Value = serde_json::from_slice(&envelope).unwrap();
        assert_eq!(
            value["error"]["message"],
            "Rate limit exceeded. Please retry after some time."
        );
    }

    #[test]
    fn capacity_signal_is_429_or_the_marked_5xx_body() {
        assert!(is_upstream_capacity_signal(429, b"anything"));
        assert!(is_upstream_capacity_signal(
            503,
            br#"{"error":"exhausted all available targets"}"#
        ));
        // A plain 5xx is an outage, not capacity.
        assert!(!is_upstream_capacity_signal(503, br#"{"error":"boom"}"#));
        // The marker under a non-5xx status is not the recognized signal.
        assert!(!is_upstream_capacity_signal(
            400,
            b"exhausted all available targets"
        ));
    }

    use super::*;
    use axum::body::to_bytes;

    async fn response_json(response: Response) -> (u16, Value) {
        let status = response.status().as_u16();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn openai_envelope_shape() {
        let (status, body) = response_json(error_response(
            Surface::Openai,
            400,
            "invalid_request_error",
            "bad",
            None,
        ))
        .await;
        assert_eq!(status, 400);
        assert_eq!(
            body,
            json!({ "error": { "message": "bad", "type": "invalid_request_error", "code": null, "param": null } })
        );
    }

    #[tokio::test]
    async fn anthropic_envelope_shape_with_request_id() {
        let (status, body) = response_json(error_response(
            Surface::Anthropic,
            404,
            "not_found_error",
            "missing",
            Some("req-1"),
        ))
        .await;
        assert_eq!(status, 404);
        assert_eq!(
            body,
            json!({
                "type": "error",
                "error": { "type": "not_found_error", "message": "missing" },
                "request_id": "req-1",
            })
        );
    }

    #[tokio::test]
    async fn rate_limit_adds_openai_code_and_headers() {
        let response = rate_limit_response(Surface::Openai, "slow down", 100, 4_000_000_000, None);
        assert_eq!(response.status().as_u16(), 429);
        assert_eq!(response.headers().get("x-ratelimit-limit").unwrap(), "100");
        assert_eq!(
            response.headers().get("x-ratelimit-remaining").unwrap(),
            "0"
        );
        let (_, body) = response_json(response).await;
        assert_eq!(body["error"]["code"], json!("rate_limit_exceeded"));
    }

    #[test]
    fn status_tables() {
        assert_eq!(error_type(Surface::Anthropic, 402), "billing_error");
        assert_eq!(error_type(Surface::Openai, 402), "insufficient_quota");
        assert_eq!(error_type(Surface::Anthropic, 500), "api_error");
        assert_eq!(error_type(Surface::Openai, 500), "upstream_error");
        assert_eq!(map_upstream_status(401), 502);
        assert_eq!(map_upstream_status(422), 422);
        assert_eq!(map_upstream_status(503), 503);
        assert!(is_actionable_client_error(400));
        assert!(!is_actionable_client_error(401));
        assert!(!is_actionable_client_error(500));
    }

    #[tokio::test]
    async fn normalize_surfaces_actionable_message_and_sanitizes_rest() {
        let (status, body) = response_json(normalize_upstream_error(
            Surface::Openai,
            400,
            br#"{"error":{"message":"missing field foo"}}"#,
            b"",
            None,
        ))
        .await;
        assert_eq!(status, 400);
        assert_eq!(body["error"]["message"], json!("missing field foo"));

        let (status, body) = response_json(normalize_upstream_error(
            Surface::Openai,
            500,
            br#"{"error":{"message":"upstream secret"}}"#,
            b"",
            None,
        ))
        .await;
        assert_eq!(status, 502);
        assert_eq!(
            body["error"]["message"],
            json!("The upstream provider returned an error")
        );
    }

    #[tokio::test]
    async fn capacity_exhaustion_5xx_remaps_to_429() {
        // An upstream 5xx that reports it has no available capacity/targets is a
        // busy signal (the message sits under `detail`, outside the usual
        // `error` field): the client sees 429 (rate-limited), not a 502 outage.
        let (status, body) = response_json(normalize_upstream_error(
            Surface::Openai,
            500,
            br#"{"detail":"exhausted all available targets to no avail"}"#,
            b"",
            None,
        ))
        .await;
        assert_eq!(status, 429);
        assert_eq!(body["error"]["type"], json!("rate_limit_error"));

        // Regression guard: a plain 500 without the marker still maps to 502.
        let (status, _) = response_json(normalize_upstream_error(
            Surface::Openai,
            500,
            br#"{"error":{"message":"kaboom"}}"#,
            b"",
            None,
        ))
        .await;
        assert_eq!(status, 502);
    }

    #[test]
    fn remote_image_urls_covers_all_shapes_and_skips_data_uris() {
        // OpenAI chat (object form) + data URI skipped.
        let openai = br#"{"messages":[{"role":"user","content":[
            {"type":"text","text":"hi"},
            {"type":"image_url","image_url":{"url":"https://a.example/x.jpg"}},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}
        ]}]}"#;
        assert_eq!(
            remote_image_urls(openai),
            vec!["https://a.example/x.jpg".to_string()]
        );
        // Anthropic native: image `source` of type url; base64 source skipped.
        let anthropic = br#"{"messages":[{"role":"user","content":[
            {"type":"image","source":{"type":"url","url":"https://a.example/anthropic.jpg"}},
            {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AAAA"}}
        ]}]}"#;
        assert_eq!(
            remote_image_urls(anthropic),
            vec!["https://a.example/anthropic.jpg".to_string()]
        );
        // Responses: input[] parts nested under content and directly, image_url as a
        // bare string.
        let responses = br#"{"input":[
            {"role":"user","content":[{"type":"input_image","image_url":"https://a.example/nested.jpg"}]},
            {"type":"input_image","image_url":"https://a.example/direct.jpg"}
        ]}"#;
        assert_eq!(
            remote_image_urls(responses),
            vec![
                "https://a.example/nested.jpg".to_string(),
                "https://a.example/direct.jpg".to_string()
            ]
        );
        // No image content at all -> empty (and the cheap guard skips the parse).
        assert!(remote_image_urls(br#"{"messages":[{"role":"user","content":"hi"}]}"#).is_empty());
    }

    #[test]
    fn classify_image_input_error_matches_url_and_host() {
        // A request carrying one remote image URL.
        fn request(url: &str) -> Vec<u8> {
            serde_json::to_vec(&json!({
                "messages": [{ "role": "user", "content": [
                    { "type": "image_url", "image_url": { "url": url } }
                ]}]
            }))
            .unwrap()
        }
        let req = request("https://halleonard.example/wl/02116757-wl.jpg");
        // Full-URL match (the 403-fetch probe).
        assert!(classify_image_input_error(
            &req,
            500,
            br#"{"error":{"message":"403, message='Forbidden', url='https://halleonard.example/wl/02116757-wl.jpg'"}}"#,
        )
        .is_some());
        // Host-only match (the DNS-failure probe).
        assert!(classify_image_input_error(
            &request("https://files.teleclaw.io/workspace/x.jpg"),
            500,
            br#"{"error":{"message":"Cannot connect to host files.teleclaw.io:443 ssl:default [Name or service not known]"}}"#,
        )
        .is_some());
        // No remote URL in the request (invalid base64 probe) -> not an image-input error.
        assert!(classify_image_input_error(
            &request("data:image/png;base64,bm90YW5pbWFnZQ=="),
            400,
            br#"{"error":{"message":"Failed to load image: cannot identify image file"}}"#,
        )
        .is_none());
        // A 5xx that names an unrelated URL must not be misclassified.
        assert!(classify_image_input_error(
            &req,
            500,
            br#"{"error":{"message":"internal error talking to https://other.example/foo"}}"#,
        )
        .is_none());
        // A bare single-label host must NOT match an unrelated word in a generic
        // provider error (the `contains(host)` false positive).
        assert!(classify_image_input_error(
            &request("https://internal/x.jpg"),
            500,
            br#"{"error":{"message":"internal error"}}"#,
        )
        .is_none());
        // A domain-like host must only match as a whole token, not as a substring of
        // a larger hostname.
        assert!(classify_image_input_error(
            &request("https://a.co/x.jpg"),
            500,
            br#"{"error":{"message":"failed to reach banana.com"}}"#,
        )
        .is_none());
    }
}
