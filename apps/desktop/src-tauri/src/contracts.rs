//! Shapes shared with the renderer (mirrored in `src/shared/contracts.ts`).

use std::collections::BTreeSet;

pub use desktop_gateway::agents::{AgentPreview, AgentStatus, ConfigChange, ConnectOptions};
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
    pub method: String,
    pub path: String,
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
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
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

impl CatalogSummary {
    pub fn from_catalog(catalog: &Catalog, previous: Option<&CatalogSummary>) -> Self {
        let models: Vec<ModelSummary> = catalog
            .models
            .iter()
            .map(|model| ModelSummary {
                id: model.id().to_string(),
                name: model.display_name().to_string(),
                context_length: model.remote.context_length,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The configuration the next start (window or tray toggle) will use.
    pub config: StartGatewayConfig,
    pub api_key_saved: bool,
    /// The catalog of the current verified session; absent whenever the
    /// session is not verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogSummary>,
}

impl Default for GatewayState {
    fn default() -> Self {
        Self {
            status: "stopped".to_string(),
            progress: None,
            remote_url: None,
            proxy_url: None,
            endpoint_error: None,
            identity: None,
            checks: Vec::new(),
            activity: Vec::new(),
            error: None,
            config: StartGatewayConfig::default(),
            api_key_saved: false,
            catalog: None,
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
            require_production_os: false,
        }
    }
}
