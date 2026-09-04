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

/// Upstream parameter shape used for chat reasoning controls.
///
/// OpenAI Chat Completions uses `reasoning_effort`. Some OpenAI-compatible
/// providers instead expose a richer nested `reasoning` object, so candidates
/// can opt into that dialect explicitly.
///
/// `thinking_type` is DeepSeek's shape: `thinking: {"type": "enabled" |
/// "disabled"}` is the switch and `reasoning_effort` the level.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
pub enum ReasoningFormat {
    #[serde(rename = "reasoning_effort")]
    ReasoningEffort,
    #[serde(rename = "reasoning")]
    Reasoning,
    #[serde(rename = "chat_template_thinking")]
    ChatTemplateThinking,
    #[serde(rename = "chat_template_enable_thinking")]
    ChatTemplateEnableThinking,
    #[serde(rename = "thinking_type")]
    ThinkingType,
}

/// Canonical public/control reasoning effort.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Max,
    Xhigh,
    High,
    Medium,
    Low,
    Minimal,
    None,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Xhigh => "xhigh",
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::Minimal => "minimal",
            Self::None => "none",
        }
    }
}

/// Route-relevant reasoning; response visibility remains gateway-local.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasoningConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<ReasoningEffort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// Billing mode, carried from the pre-consult into the post-consult report.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpendMode {
    Regular,
    Subscription,
    SubscriptionOverflow,
}

/// Deployment-level reasoning policy, read from config by the control plane
/// and passed to the gateway verbatim. The gateway owns the decision logic —
/// it has the request context (response_format, tools, max_tokens) needed to
/// choose which field applies.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasoningPolicy {
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "override")]
    pub override_policy: Option<ReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "default")]
    pub default_policy: Option<ReasoningConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<u64>,
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
    /// Parameter dialect accepted by this route. When omitted, the gateway
    /// preserves the legacy inference: managed routes use `reasoning`, while
    /// self-hosted engines use `reasoning_effort`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_format: Option<ReasoningFormat>,
    /// Raw reasoning policy from deployment config; the gateway decides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_policy: Option<ReasoningPolicy>,
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

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrganizationScope {
    pub organization_id: i64,
    pub workspace_id: i64,
}

/// Authenticated actor and optional resource scope fixed by pre-consult.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    #[serde(flatten)]
    pub organization: Option<OrganizationScope>,
}

/// Pre-request consult response. On `allow: false`, `status` and `message` carry
/// the client-facing denial; otherwise `candidates` and `pricing` drive routing.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", try_from = "PreConsultWire")]
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
    #[serde(flatten)]
    pub tenant: TenantIdentity,
    #[serde(default)]
    pub virtual_key_id: Option<i64>,
    #[serde(default)]
    pub spend_mode: Option<SpendMode>,
    #[serde(default)]
    pub user_tier: Option<String>,
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PreConsultWire {
    allow: bool,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    pricing: Option<PricingConfig>,
    #[serde(default)]
    candidates: Option<Vec<RouteCandidate>>,
    #[serde(default)]
    user_id: Option<i64>,
    #[serde(default)]
    organization_id: Option<i64>,
    #[serde(default)]
    workspace_id: Option<i64>,
    #[serde(default)]
    virtual_key_id: Option<i64>,
    #[serde(default)]
    spend_mode: Option<SpendMode>,
    #[serde(default)]
    user_tier: Option<String>,
    #[serde(default)]
    rate_limit: Option<RateLimit>,
}

impl TryFrom<PreConsultWire> for PreConsult {
    type Error = &'static str;

    fn try_from(wire: PreConsultWire) -> Result<Self, Self::Error> {
        let user_id = match wire.user_id {
            Some(user_id) if user_id > 0 => Some(user_id),
            None => None,
            Some(_) => return Err("userId must be a positive integer when provided"),
        };
        let organization = match (wire.organization_id, wire.workspace_id) {
            (Some(organization_id), Some(workspace_id))
                if organization_id > 0 && workspace_id > 0 =>
            {
                Some(OrganizationScope {
                    organization_id,
                    workspace_id,
                })
            }
            (None, None) => None,
            _ => {
                return Err("organizationId and workspaceId must be positive and provided together")
            }
        };
        if organization.is_some() && user_id.is_none() {
            return Err("organizationId and workspaceId require a positive userId");
        }

        Ok(Self {
            allow: wire.allow,
            status: wire.status,
            message: wire.message,
            pricing: wire.pricing,
            candidates: wire.candidates,
            tenant: TenantIdentity {
                user_id,
                organization,
            },
            virtual_key_id: wire.virtual_key_id,
            spend_mode: wire.spend_mode,
            user_tier: wire.user_tier,
            rate_limit: wire.rate_limit,
        })
    }
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
    #[serde(flatten)]
    pub tenant: TenantIdentity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub virtual_key_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_source: Option<ErrorSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Echo of the pre-consult request features' prefix hash, so billing can
    /// record which deployment actually served this prefix (cache affinity).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::PreConsult;

    #[test]
    fn tenant_identity_accepts_legacy_and_expanded_scopes() {
        let legacy: PreConsult = serde_json::from_value(serde_json::json!({
            "allow": true,
            "userId": 7
        }))
        .unwrap();
        assert_eq!(legacy.tenant.user_id, Some(7));
        assert!(legacy.tenant.organization.is_none());

        let user_with_resources: PreConsult = serde_json::from_value(serde_json::json!({
            "allow": true,
            "userId": 7,
            "organizationId": 11,
            "workspaceId": 13
        }))
        .unwrap();
        assert_eq!(user_with_resources.tenant.user_id, Some(7));
        assert_eq!(
            user_with_resources
                .tenant
                .organization
                .unwrap()
                .organization_id,
            11
        );

        let anonymous: PreConsult = serde_json::from_value(serde_json::json!({
            "allow": false
        }))
        .unwrap();
        assert!(anonymous.tenant.user_id.is_none());
        assert!(anonymous.tenant.organization.is_none());
    }

    #[test]
    fn tenant_identity_rejects_invalid_wire_shapes() {
        for invalid in [
            serde_json::json!({ "allow": true, "userId": 0 }),
            serde_json::json!({
                "allow": true,
                "userId": 7,
                "organizationId": 11
            }),
            serde_json::json!({
                "allow": true,
                "userId": 7,
                "organizationId": 11,
                "workspaceId": 0
            }),
            serde_json::json!({
                "allow": true,
                "organizationId": 11,
                "workspaceId": 13
            }),
        ] {
            assert!(serde_json::from_value::<PreConsult>(invalid).is_err());
        }
    }
}
