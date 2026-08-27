//! Control-plane consult types.
//!
//! The control plane speaks a camelCase wire shape; these structs mirror it so a
//! pre-consult response deserializes and a post-consult report serializes without
//! hand-built JSON. Pricing is carried as an opaque value, interpreted by the
//! cost computation.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

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

/// Ledger owner selected by the control plane. User is the legacy account;
/// organization is the tenant ledger. The gateway only echoes it.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BillingOwnerType {
    User,
    Organization,
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
    pub organization_id: Option<i64>,
    #[serde(default)]
    pub workspace_id: Option<Uuid>,
    #[serde(default)]
    pub billing_owner_type: Option<BillingOwnerType>,
    #[serde(default)]
    pub billing_owner_id: Option<i64>,
    #[serde(default)]
    pub virtual_key_id: Option<i64>,
    #[serde(default)]
    pub spend_mode: Option<SpendMode>,
    #[serde(default)]
    pub user_tier: Option<String>,
    #[serde(default)]
    pub rate_limit: Option<RateLimit>,
}

impl PreConsult {
    /// Expand the legacy User-only wire shape into the tagged billing identity.
    /// Remove after every control deployment emits `billingOwnerType`.
    pub fn normalize_legacy_user_billing_identity(&mut self) {
        if self.billing_owner_type.is_some()
            || self.billing_owner_id.is_some()
            || self.organization_id.is_some()
            || self.workspace_id.is_some()
        {
            return;
        }
        if let (Some(user_id), Some(virtual_key_id)) = (self.user_id, self.virtual_key_id) {
            if user_id > 0 && virtual_key_id > 0 {
                self.billing_owner_type = Some(BillingOwnerType::User);
                self.billing_owner_id = Some(user_id);
            }
        }
    }

    /// Validate the complete billing identity before the response is used. This
    /// catches mixed-version control responses before forwarding or reporting.
    pub fn has_consistent_billing_identity(&self) -> bool {
        match self.user_id {
            None => {
                self.organization_id.is_none()
                    && self.workspace_id.is_none()
                    && self.billing_owner_type.is_none()
                    && self.billing_owner_id.is_none()
                    && self.virtual_key_id.is_none()
            }
            Some(user_id) if user_id > 0 => {
                if self.virtual_key_id.is_none_or(|id| id <= 0) {
                    return false;
                }
                match (self.billing_owner_type, self.billing_owner_id) {
                    (Some(BillingOwnerType::User), Some(owner_id)) => {
                        owner_id == user_id
                            && self.organization_id.is_none()
                            && self.workspace_id.is_none()
                    }
                    (Some(BillingOwnerType::Organization), Some(owner_id)) => {
                        owner_id > 0
                            && self.organization_id == Some(owner_id)
                            && self.workspace_id.is_some()
                    }
                    _ => false,
                }
            }
            Some(_) => false,
        }
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_owner_type: Option<BillingOwnerType>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_owner_id: Option<i64>,
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
    fn organization_identity_requires_complete_workspace_scope() {
        let complete: PreConsult = serde_json::from_value(serde_json::json!({
            "allow": true,
            "userId": 7,
            "organizationId": 11,
            "workspaceId": "018f3e7c-8d2d-7e5a-9f23-31d2a7c48810",
            "billingOwnerType": "organization",
            "billingOwnerId": 11,
            "virtualKeyId": 3
        }))
        .unwrap();
        assert!(complete.has_consistent_billing_identity());

        let incomplete: PreConsult = serde_json::from_value(serde_json::json!({
            "allow": true,
            "userId": 7,
            "organizationId": 11,
            "billingOwnerType": "organization",
            "billingOwnerId": 11,
            "virtualKeyId": 3
        }))
        .unwrap();
        assert!(!incomplete.has_consistent_billing_identity());
    }

    #[test]
    fn legacy_user_and_anonymous_identities_remain_valid() {
        let mut legacy: PreConsult = serde_json::from_value(serde_json::json!({
            "allow": true,
            "userId": 7,
            "virtualKeyId": 3
        }))
        .unwrap();
        legacy.normalize_legacy_user_billing_identity();
        assert!(legacy.has_consistent_billing_identity());
        assert_eq!(
            legacy.billing_owner_type,
            Some(super::BillingOwnerType::User)
        );
        assert_eq!(legacy.billing_owner_id, Some(7));

        let mut denied: PreConsult = serde_json::from_value(serde_json::json!({
            "allow": false,
            "status": 429,
            "userId": 7,
            "virtualKeyId": 3
        }))
        .unwrap();
        denied.normalize_legacy_user_billing_identity();
        assert!(denied.has_consistent_billing_identity());
        assert_eq!(
            denied.billing_owner_type,
            Some(super::BillingOwnerType::User)
        );
        assert_eq!(denied.billing_owner_id, Some(7));

        let anonymous: PreConsult =
            serde_json::from_value(serde_json::json!({ "allow": true })).unwrap();
        assert!(anonymous.has_consistent_billing_identity());
    }

    #[test]
    fn partial_organization_identity_is_never_normalized_as_a_user() {
        let mut partial: PreConsult = serde_json::from_value(serde_json::json!({
            "allow": true,
            "userId": 7,
            "organizationId": 11,
            "virtualKeyId": 3
        }))
        .unwrap();

        partial.normalize_legacy_user_billing_identity();

        assert!(!partial.has_consistent_billing_identity());
    }

    #[test]
    fn invalid_workspace_id_is_rejected_at_the_wire_boundary() {
        let result = serde_json::from_value::<PreConsult>(serde_json::json!({
            "allow": true,
            "userId": 7,
            "organizationId": 11,
            "workspaceId": "not-a-uuid",
            "billingOwnerType": "organization",
            "billingOwnerId": 11,
            "virtualKeyId": 3
        }));

        assert!(result.is_err());
    }
}
