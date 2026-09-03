//! Shapes shared with the renderer (mirrored in `src/shared/contracts.ts`).

use std::collections::BTreeSet;

pub use desktop_gateway::agents::{AgentPreview, AgentStatus, ConnectOptions};
use desktop_gateway::catalog::Catalog;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCheck {
    pub id: String,
    pub section: String,
    pub title: String,
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayIdentity {
    pub tee_type: String,
    pub trust_level: String,
    pub keyset_digest: String,
    pub keyset_not_after: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tls_spki: Option<String>,
    pub source: SourceProvenance,
    pub serving: String,
    pub supported_e2ee_versions: Vec<String>,
}

/// One request seen by the local gateway: forwarded through the sidecar (with
/// its receipt verdict) or answered locally (rejected before any receipt).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestActivity {
    pub id: String,
    pub session_id: String,
    pub method: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub status: u16,
    pub streamed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receipt_id: Option<String>,
    pub verified: Option<bool>,
    pub detail: String,
    pub at: u64,
    /// The connected agent that sent it, when it presented a token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Whether the verifier applied its ACI policy to the body before
    /// forwarding; the receipt binds those bytes, not the agent's original.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locally_constrained: Option<bool>,
    /// Whether the receipt records a service-side rewrite of the request.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rewritten: Option<bool>,
    /// False means the request was rejected by the local proxy before any
    /// bytes left the device.
    pub left_device: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost_usd: f64,
    pub protected: u64,
    pub blocked_locally: u64,
    pub failed_proof: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_tee: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_price_per_million: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_price_per_million: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_price_per_million: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_price_per_million: Option<f64>,
    pub input_modalities: Vec<String>,
    pub output_modalities: Vec<String>,
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSummary {
    pub revision: String,
    pub fetched_at: u64,
    pub models: Vec<ModelSummary>,
    /// Ids served by an earlier refresh that the service no longer lists.
    pub removed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceProvider {
    Phala,
    Redpill,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProfileAuth {
    #[serde(rename = "apiKey")]
    ApiKey,
    #[serde(rename = "oauth")]
    OAuth {
        #[serde(rename = "accountId")]
        account_id: String,
        #[serde(
            rename = "accountName",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        account_name: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfidentialProfile {
    pub id: String,
    pub name: String,
    pub provider: ServiceProvider,
    pub remote_url: String,
    pub auth: ProfileAuth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfidentialProfileInput {
    pub id: String,
    pub name: String,
    pub provider: ServiceProvider,
    pub remote_url: String,
}

impl CatalogSummary {
    pub fn from_catalog(catalog: &Catalog, previous: Option<&CatalogSummary>) -> Self {
        let models: Vec<ModelSummary> = catalog
            .models
            .iter()
            .map(|model| ModelSummary {
                id: model.id().to_string(),
                name: model.display_name().to_string(),
                context_length: model.remote.context_length,
                max_output_length: model.remote.max_output_length,
                is_tee: model.bool_field("is_tee"),
                input_price_per_million: model.price_per_million("prompt"),
                output_price_per_million: model.price_per_million("completion"),
                cache_read_price_per_million: model.price_per_million("input_cache_read"),
                cache_write_price_per_million: model.price_per_million("input_cache_write"),
                input_modalities: model.string_array("input_modalities"),
                output_modalities: model.string_array("output_modalities"),
                capabilities: model.string_array("supported_features"),
                description: model.string_field("description"),
            })
            .collect();
        // Carry forward ids that disappeared until the service lists them
        // again, so a removed model is never quietly forgotten.
        let removed: BTreeSet<String> = previous
            .into_iter()
            .flat_map(|previous| {
                previous
                    .models
                    .iter()
                    .map(|model| model.id.clone())
                    .chain(previous.removed.iter().cloned())
            })
            .filter(|id| catalog.get(id).is_none())
            .collect();
        Self {
            revision: catalog.revision.clone(),
            fetched_at: catalog.fetched_at,
            models,
            removed: removed.into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayState {
    /// `stopped`, `verifying` (identity and catalog not both in), `verified`,
    /// `blocked`, or `error`.
    pub status: String,
    /// True while Settings is verifying a candidate configuration without
    /// opening the forwarding session or turning protection on.
    pub configuration_verification: bool,
    /// What the gateway is doing while `verifying`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// The stable local endpoint agents use; present only while it is bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
    /// Why the local endpoint could not be bound; blocks starting and connecting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<GatewayIdentity>,
    pub checks: Vec<VerificationCheck>,
    pub activity: Vec<RequestActivity>,
    /// Stable id and complete persisted totals for the current protection run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub session_usage: UsageSummary,
    /// Changes only when persisted usage changes; renderer queries can depend
    /// on this instead of the bounded activity preview.
    pub usage_revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The configuration the next start (window or tray toggle) will use.
    pub config: StartGatewayConfig,
    pub profiles: Vec<ConfidentialProfile>,
    pub active_profile_id: String,
    pub local_api: LocalApiConfig,
    pub api_key_saved: bool,
    /// The most recently verified catalog. A stopped gateway may retain it so
    /// Settings can show what a successful configuration check discovered;
    /// the proxy still requires a live verified session before forwarding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogSummary>,
}

impl Default for GatewayState {
    fn default() -> Self {
        Self {
            status: "stopped".to_string(),
            configuration_verification: false,
            progress: None,
            remote_url: None,
            proxy_url: None,
            endpoint_error: None,
            identity: None,
            checks: Vec::new(),
            activity: Vec::new(),
            session_id: None,
            session_usage: UsageSummary::default(),
            usage_revision: 0,
            error: None,
            config: StartGatewayConfig::default(),
            profiles: Vec::new(),
            active_profile_id: String::new(),
            local_api: LocalApiConfig::default(),
            api_key_saved: false,
            catalog: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalApiConfig {
    pub listen_address: String,
    pub allow_network_access: bool,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_host: Option<String>,
}

impl Default for LocalApiConfig {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1".to_string(),
            allow_network_access: false,
            port: 4180,
            client_host: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartGatewayConfig {
    pub remote_url: String,
    pub require_production_os: bool,
}

impl Default for StartGatewayConfig {
    fn default() -> Self {
        Self {
            remote_url: desktop_gateway::brand::SERVICE_DEFAULT_URL.to_string(),
            require_production_os: true,
        }
    }
}
