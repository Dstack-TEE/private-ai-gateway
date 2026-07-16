//! Configuration for the middleware.
//!
//! Selected through the gateway's optional `middleware` config section.

use serde::Deserialize;

/// Which middleware backend owns the request path.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MiddlewareMode {
    /// Existing control-plane mode: PAG asks `/consult/pre` for ordered
    /// candidates and forwards through its own backend path.
    Control,
    /// Data-plane proxy mode: PAG sends the full normalized request to an
    /// external middleware, which calls back to PAG `/internal/forward` with
    /// the selected route.
    Proxy,
}

impl Default for MiddlewareMode {
    fn default() -> Self {
        Self::Control
    }
}

/// Middleware settings. Existing `control_url` configs remain valid and default
/// to `mode = "control"`. `mode = "proxy"` is for request-body-aware
/// middlewares such as a vLLM Router wrapper.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiddlewareConfig {
    #[serde(default)]
    pub mode: MiddlewareMode,
    /// Base URL of the control plane (`http`/`https`). Consult and catalog paths
    /// are appended to it. Required in `control` mode.
    #[serde(default)]
    pub control_url: Option<String>,
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
    /// Base URL of the data-plane middleware. Required in `proxy` mode.
    #[serde(default)]
    pub proxy_url: Option<String>,
    /// Bearer-like shared secret used only on the internal callback from the
    /// data-plane middleware to `POST /internal/forward`. Required in `proxy`
    /// mode.
    #[serde(default)]
    pub internal_token: Option<String>,
    /// Timeout for the external proxy request. Defaults to 1_800_000 ms.
    #[serde(default)]
    pub proxy_timeout_ms: Option<u64>,
}
