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
    /// Hosts (matched against the request `Host` header) that serve TEE models
    /// only. On these hosts the model catalog is forced to `?tee=true`,
    /// non-TEE models are refused (404) at consult, and serving is forced to
    /// attested (`aci_verified`) upstreams — a client cannot opt out. Empty
    /// (the default) leaves every host unrestricted.
    #[serde(default)]
    pub tee_only_domains: Vec<String>,
}
