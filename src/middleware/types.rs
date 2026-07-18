//! Control-plane consult types.
//!
//! The control plane speaks a camelCase wire shape; these structs mirror it so a
//! pre-consult response deserializes and a post-consult report serializes without
//! hand-built JSON. Pricing is carried as an opaque value, interpreted by the
//! cost computation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Opaque pricing block. Carried verbatim until cost computation lands.
pub type PricingConfig = Value;

/// Which API format shapes a candidate's request and parses its response.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderFormat {
    Openai,
    Anthropic,
}

/// Serving engine of a self-hosted OpenAI-compatible upstream. Selects
/// engine-specific request shaping; absent for managed third-party APIs.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Engine {
    Sglang,
    Vllm,
}

/// Billing mode, carried from the pre-consult into the post-consult report.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpendMode {
    Regular,
    Subscription,
    SubscriptionOverflow,
}

/// One ordered failover candidate: a backend route id plus the upstream format.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RouteCandidate {
    /// `<provider>:<public model id>`, aligned with the backend's upstreams.
    pub route_id: String,
    /// API format that shapes the request and parses the response.
    pub format: ProviderFormat,
    /// Serving engine when this upstream is a self-hosted OpenAI-compatible
    /// server. Absent for managed APIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<Engine>,
}

/// Provider routing block, forwarded verbatim to the control plane.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProviderRouting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub only: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_fallbacks: Option<bool>,
}

/// Rate-limit hint set on a 429 denial; drives the `X-RateLimit-*` headers.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimit {
    pub limit: i64,
    pub reset_at: i64,
}

/// Pre-request consult response. On `allow: false`, `status` and `message` carry
/// the client-facing denial; otherwise `candidates` and `pricing` drive routing.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreConsult {
    pub allow: bool,
    #[serde(default)]
    pub status: Option<u16>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub pricing: Option<PricingConfig>,
    #[serde(default)]
    pub candidates: Option<Vec<RouteCandidate>>,
    #[serde(default)]
    pub user_id: Option<i64>,
    #[serde(default)]
    pub virtual_key_id: Option<i64>,
    #[serde(default)]
    pub spend_mode: Option<SpendMode>,
    #[serde(default)]
    pub user_tier: Option<String>,
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
}

/// Which component a gateway-synthesized failure (no real upstream attempt) is
/// attributed to. Drives the control plane's error-source column: `control`
/// (control-plane consult), `upstream` (provider forwarding/verification or a
/// malformed upstream success body), or `gateway` (the gateway's own logic).
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ErrorSource {
    Control,
    Upstream,
    Gateway,
}

/// Post-request usage report. Fire-and-forget; drives billing and request logs.
///
/// `selected_route_id`, `usage`, and `pricing` are always present (serialized as
/// `null` when absent) to match the control plane's expected shape; the rest are
/// omitted when unset.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostReport {
    pub request_id: String,
    pub endpoint: String,
    pub status: u16,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_streaming: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempt_index: Option<u32>,
    /// `<provider>:<model>` from the backend's selected route, or `null`.
    ///
    /// Wire contract for consumers: a request may emit multiple per-attempt
    /// reports — aggregate by `request_id`. A report with
    /// `selected_route_id == null` and a non-empty `error_source` is a
    /// request-level summary (e.g. the aggregate error after every candidate
    /// failed), not an attempt; attempt counting must only consider reports
    /// that carry a route.
    pub selected_route_id: Option<String>,
    pub request_model: String,
    /// Raw upstream usage before any cost injection, or `null`.
    pub usage: Option<Value>,
    pub pricing: Option<PricingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_mode: Option<SpendMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtual_key_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_source: Option<ErrorSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}
