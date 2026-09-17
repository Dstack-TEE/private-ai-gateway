//! Control-plane consult types.
//!
//! The control plane speaks a camelCase wire shape; these structs mirror it so a
//! pre-consult response deserializes and a post-consult report serializes without
//! hand-built JSON. Pricing is carried as an opaque value, interpreted by the
//! cost computation.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::aci::upstream::UpstreamError;
use crate::aggregator::service::{ServiceError, UpstreamVerificationError};

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
    /// Omit both OpenAI output-limit aliases for structured chat output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omit_max_tokens: Option<bool>,
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
    /// API paths implemented directly by the upstream. Endpoints omitted here
    /// may still be served through a gateway conversion when one exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supported_endpoints: Vec<String>,
}

impl RouteCandidate {
    pub fn supports_endpoint(&self, path: &str) -> bool {
        self.supported_endpoints
            .iter()
            .any(|supported| supported == path)
    }
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

/// Anonymous requests omit identity; authenticated actors carry their resource scope.
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
        if organization.is_some() != user_id.is_some() {
            return Err("userId, organizationId and workspaceId must be provided together");
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

/// Why a request or attempt failed, as a closed vocabulary.
///
/// This is the only description of a failure that a usage report can carry.
/// No variant holds a string, so an upstream response body, a provider's error prose, and anything a caller sent
/// cannot be recorded through it: the compiler rejects the attempt. Choosing
/// a variant may read an upstream body — for a provider-declared kind, a fixed
/// capacity marker, or the caller's own image URL — but what is kept is the
/// variant alone.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// The control plane refused the request.
    ControlDenied,
    /// The control plane could not be consulted.
    ControlUnavailable,
    /// The control plane offered no route for the requested model.
    ModelNotFound,
    /// No candidate route could be given a provider-shaped request body.
    RequestShapingFailed,
    /// No attested route or session was eligible; no prompt was forwarded.
    NoEligibleAttestedRoute,
    /// The upstream answered with a non-2xx status.
    UpstreamHttpError,
    /// The upstream declared that this gateway's account with it is unpaid.
    UpstreamQuotaExhausted,
    /// The upstream rate-limited the gateway or signalled it has no capacity.
    UpstreamCapacity,
    /// The upstream could not fetch an image URL the caller supplied.
    UpstreamImageFetchFailed,
    /// The upstream did not answer before the gateway's deadline.
    UpstreamTimeout,
    /// The connection to the upstream failed.
    UpstreamTransport,
    /// The upstream's channel did not match its verified binding.
    UpstreamChannelBindingMismatch,
    /// The upstream could not be verified.
    UpstreamVerificationFailed,
    /// The upstream answered 2xx with a body the gateway could not use.
    UpstreamMalformedResponse,
    /// The upstream answered 2xx with a Responses envelope whose `status` is
    /// `failed`.
    UpstreamResponseFailed,
    /// A 2xx stream carried an error event.
    StreamInbandError,
    /// A 2xx stream ended without its terminal marker.
    StreamTruncated,
    /// A stream line exceeded the gateway's size cap.
    StreamLineOverflow,
    /// The caller disconnected before the response completed.
    ClientDisconnected,
    /// End-to-end encryption of the request or response failed.
    E2eeFailed,
    /// The receipt for the response could not be produced.
    ReceiptFailed,
    /// The response could not be finalized after the upstream completed.
    DownstreamFinalizerFailed,
    /// A failure inside the gateway not covered above.
    InternalError,
}

impl From<&UpstreamError> for ErrorClass {
    fn from(err: &UpstreamError) -> Self {
        match err {
            UpstreamError::Routing(_) => Self::InternalError,
            UpstreamError::Transport(_) => Self::UpstreamTransport,
            UpstreamError::Timeout(_) => Self::UpstreamTimeout,
            UpstreamError::ChannelBindingMismatch(_) => Self::UpstreamChannelBindingMismatch,
            UpstreamError::Upstream { .. } => Self::UpstreamHttpError,
        }
    }
}

impl From<&ServiceError> for ErrorClass {
    fn from(err: &ServiceError) -> Self {
        match err {
            ServiceError::UpstreamVerification(
                UpstreamVerificationError::NoEligibleAttestedRoute(_)
                | UpstreamVerificationError::NoEligibleAttestedSession(_),
            ) => Self::NoEligibleAttestedRoute,
            ServiceError::UpstreamVerification(
                UpstreamVerificationError::NoVerifierResult
                | UpstreamVerificationError::VerifierFailed(_),
            ) => Self::UpstreamVerificationFailed,
            ServiceError::Upstream(upstream) => upstream.into(),
            ServiceError::E2ee(_) => Self::E2eeFailed,
            ServiceError::Receipt(_) | ServiceError::NoReceiptKey => Self::ReceiptFailed,
            ServiceError::TestKeysInProduction
            | ServiceError::InvalidSourceProvenance
            | ServiceError::Keyset(_)
            | ServiceError::InvalidNonce(_)
            | ServiceError::Key(_)
            | ServiceError::SessionStore(_)
            | ServiceError::Metrics(_)
            | ServiceError::DownstreamTlsDomainMissing
            | ServiceError::DownstreamTlsDomainUnknown(_) => Self::InternalError,
        }
    }
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
    /// Serialized as `errorMessage`: the class's fixed token, never free text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<ErrorClass>,
    /// Echo of the pre-consult request features' prefix hash, so billing can
    /// record which deployment actually served this prefix (cache affinity).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefix_hash: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::{ErrorClass, PreConsult};

    /// The full reporting vocabulary. A report's `errorMessage` is always one
    /// of these tokens; growing the list is a deliberate, reviewed change.
    #[test]
    fn error_class_serializes_to_its_fixed_token() {
        let vocabulary = [
            (ErrorClass::ControlDenied, "control_denied"),
            (ErrorClass::ControlUnavailable, "control_unavailable"),
            (ErrorClass::ModelNotFound, "model_not_found"),
            (ErrorClass::RequestShapingFailed, "request_shaping_failed"),
            (
                ErrorClass::NoEligibleAttestedRoute,
                "no_eligible_attested_route",
            ),
            (ErrorClass::UpstreamHttpError, "upstream_http_error"),
            (
                ErrorClass::UpstreamQuotaExhausted,
                "upstream_quota_exhausted",
            ),
            (ErrorClass::UpstreamCapacity, "upstream_capacity"),
            (
                ErrorClass::UpstreamImageFetchFailed,
                "upstream_image_fetch_failed",
            ),
            (ErrorClass::UpstreamTimeout, "upstream_timeout"),
            (ErrorClass::UpstreamTransport, "upstream_transport"),
            (
                ErrorClass::UpstreamChannelBindingMismatch,
                "upstream_channel_binding_mismatch",
            ),
            (
                ErrorClass::UpstreamVerificationFailed,
                "upstream_verification_failed",
            ),
            (
                ErrorClass::UpstreamMalformedResponse,
                "upstream_malformed_response",
            ),
            (
                ErrorClass::UpstreamResponseFailed,
                "upstream_response_failed",
            ),
            (ErrorClass::StreamInbandError, "stream_inband_error"),
            (ErrorClass::StreamTruncated, "stream_truncated"),
            (ErrorClass::StreamLineOverflow, "stream_line_overflow"),
            (ErrorClass::ClientDisconnected, "client_disconnected"),
            (ErrorClass::E2eeFailed, "e2ee_failed"),
            (ErrorClass::ReceiptFailed, "receipt_failed"),
            (
                ErrorClass::DownstreamFinalizerFailed,
                "downstream_finalizer_failed",
            ),
            (ErrorClass::InternalError, "internal_error"),
        ];
        for (class, token) in vocabulary {
            assert_eq!(
                serde_json::to_value(class).unwrap(),
                serde_json::json!(token)
            );
        }
    }

    #[test]
    fn tenant_identity_accepts_scoped_and_anonymous_requests() {
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
            serde_json::json!({ "allow": true, "userId": 7 }),
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
