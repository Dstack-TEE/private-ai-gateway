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
    /// SSE keep-alive interval for streaming responses. Defaults to 10_000 ms;
    /// `0` disables the heartbeat.
    #[serde(default)]
    pub sse_keepalive_ms: Option<u64>,
    /// Whether the keep-alive also covers the wait for the upstream's response
    /// headers: a streaming request with no upstream answer after one
    /// `sse_keepalive_ms` interval is committed as `200 text/event-stream`
    /// and heartbeated until the upstream responds; a later forward failure
    /// arrives as the surface's in-band error event. Off by default because a
    /// response committed this early carries no `x-receipt-id` header (spec
    /// §5.2 puts a receipt on every inference response; here the receipt is
    /// still issued and fetchable by the response id, but header-driven
    /// clients cannot see it). Requests carrying an ACI constraint
    /// (`provider.aci_verified` / pinned session ids) are never committed
    /// early even when enabled, preserving refusal-receipt semantics.
    #[serde(default)]
    pub sse_commit_before_upstream: Option<bool>,
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
