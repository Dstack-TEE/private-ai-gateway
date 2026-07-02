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
    /// How long a streaming request may wait for the upstream response before
    /// the gateway commits a `200 text/event-stream` to the client and starts
    /// heartbeating while the forward continues behind the live stream. Within
    /// the grace window the response keeps full status fidelity (upstream
    /// status and headers pass through); past it, upstream failures are
    /// delivered as an in-band SSE error event. Defaults to 2_000 ms; `0`
    /// disables early commit (the response always waits for the upstream).
    #[serde(default)]
    pub stream_commit_grace_ms: Option<u64>,
    /// Per-candidate deadline for a streaming attempt to produce its first
    /// body byte (covers the response-header wait too; heartbeat comments do
    /// not count). On expiry the candidate is abandoned and the next one is
    /// tried; the last candidate is never cut short. Disabled by default
    /// (`0`/unset): waiting for the first byte delays the commit decision, so
    /// enabling it routes slow-first-token streams on multi-candidate routes
    /// through the early-commit path. Enable deliberately (e.g. 60_000) for
    /// deployments whose candidates are known to stall.
    #[serde(default)]
    pub stream_first_byte_timeout_ms: Option<u64>,
}
