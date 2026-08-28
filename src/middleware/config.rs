//! Configuration for the middleware.
//!
//! Selected through the gateway's optional `middleware` config section. When
//! present, the gateway consults the control plane directly over HTTP, in
//! process, with no Unix-domain-socket hop.

use serde::Deserialize;

/// Middleware settings. `control_url` is required; the rest fall back
/// to the defaults documented in the configuration reference.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareConfig {
    /// Base URL of the control plane (`http`/`https`). Consult and catalog paths
    /// are appended to it.
    pub control_url: String,
    /// Optional bearer token for control-plane requests.
    #[serde(default)]
    pub control_token: Option<String>,
    /// Timeout for the pre-request consult and catalog fetches. Defaults to
    /// 60_000 ms.
    #[serde(default)]
    pub control_timeout_ms: Option<u64>,
    /// Timeout for the fire-and-forget post-request usage report. Defaults to
    /// 10_000 ms.
    #[serde(default)]
    pub control_post_timeout_ms: Option<u64>,
    /// SSE keep-alive interval for streaming responses, measured from the
    /// start of the upstream forward. A streaming request with no upstream
    /// response headers after one interval is committed as
    /// `200 text/event-stream` and heartbeated until the upstream answers; a
    /// later forward failure arrives as the surface's in-band error event.
    /// A response committed this early carries no `x-receipt-id` header: when
    /// the upstream answers and the stream finalizes, the receipt is issued
    /// and fetchable by the response id, but an early-committed stream whose
    /// forward fails never drafts one. Requests carrying an ACI constraint
    /// (`provider.aci_verified` — the aci CLI's default — or pinned session
    /// ids) are never committed early: their refusal-receipt and 412
    /// semantics only exist as HTTP responses. Neither is a candidate that has
    /// already failed once in this request: a same-route retry usually ends in
    /// a relayable HTTP status (429 above all), which an early 200 would
    /// demote to an in-band error. Defaults to 5_000 ms; `0` disables the
    /// heartbeat and the pre-upstream commit with it.
    #[serde(default)]
    pub sse_keepalive_ms: Option<u64>,
    /// Whether to extract content-derived request features (token estimate,
    /// modalities, reasoning intent, prefix hash — see
    /// `request_features.rs`) and send them in the pre-request consult.
    /// Defaults to on; `false` restores the featureless consult body
    /// byte-for-byte — the rollback lever if extraction ever misbehaves.
    #[serde(default)]
    pub send_request_features: Option<bool>,
    /// HMAC key for the consult prefix hash. When set, the cache-affinity key
    /// is HMAC-SHA256(secret, prefix): the control plane cannot dictionary-
    /// test guessed prompts, so the hash carries no content signal beyond
    /// equality. Must be a random value of at least 32 bytes — the gateway
    /// refuses to start on anything shorter, because HMAC under an empty or
    /// guessable key is as computable as the plain hash it claims to improve
    /// on. Every gateway replica must share the same value, or affinity
    /// silently fragments per replica. Unset falls back to plain SHA-256
    /// (equality linkable; a fully-known 4KB template is confirmable).
    #[serde(default)]
    pub prefix_hash_secret: Option<String>,
    /// Hosts (matched against the request `Host` header) that serve TEE models
    /// only. On these hosts the model catalog is forced to `?tee=true`,
    /// non-TEE models are refused (404) at consult, and serving is forced to
    /// attested (`aci_verified`) upstreams — a client cannot opt out. Empty
    /// (the default) leaves every host unrestricted.
    #[serde(default)]
    pub tee_only_domains: Vec<String>,
}
