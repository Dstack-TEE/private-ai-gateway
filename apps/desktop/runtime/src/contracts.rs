//! Shapes shared with the renderer. `src/shared/contracts.generated.ts` is
//! generated from them; run `npm run generate:contracts` after changing one.

use std::collections::BTreeSet;

pub use desktop_gateway::agents::{AgentPreview, AgentStatus, ConnectOptions};
use desktop_gateway::catalog::Catalog;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct VerificationCheck {
    pub id: String,
    pub section: String,
    pub title: String,
    #[ts(type = r#""pass" | "fail" | "skip" | "info""#)]
    pub status: String,
    pub detail: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct SourceProvenance {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
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

/// One request seen by the local gateway: forwarded through the verifier (with
/// its receipt verdict) or answered locally (rejected before any receipt).
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
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
    #[ts(optional = false)]
    pub verified: Option<bool>,
    pub detail: String,
    pub at: u64,
    /// The connected agent that sent it, when it presented a token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Whether the verifier applied its ACI policy to the body before
    /// forwarding; the receipt binds those bytes, not the agent's original.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_policy_applied: Option<bool>,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize, TS)]
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

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct ModelSummary {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_endpoints: Option<Vec<String>>,
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

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CatalogSummary {
    pub revision: String,
    pub fetched_at: u64,
    pub models: Vec<ModelSummary>,
    /// Ids served by an earlier refresh that the service no longer lists.
    pub removed: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum ServiceProvider {
    Phala,
    Redpill,
    Custom,
}

impl ServiceProvider {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Phala => "Phala",
            Self::Redpill => "RedPill",
            Self::Custom => "Custom",
        }
    }

    pub const fn preset_url(self) -> Option<&'static str> {
        match self {
            Self::Phala => Some("https://inference.phala.com"),
            Self::Redpill => Some("https://tee.redpill.ai"),
            Self::Custom => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum AccountSaveResult {
    Running,
    Complete { state: Box<GatewayState> },
    Failed { error: String },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub organization_slug: Option<String>,
    pub organization: Option<String>,
    pub workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_slug: Option<String>,
    pub workspace_id: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountWorkspace {
    pub id: i64,
    pub name: String,
    #[serde(alias = "is_default")]
    pub is_default: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountLoginDetails {
    pub auth: ProfileAuth,
    pub workspaces: Vec<AccountWorkspace>,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct AccountBalance {
    pub balance_usd: String,
    pub can_top_up: bool,
    pub organization_id: Option<String>,
    pub granted_usd: Option<String>,
    pub scope: AccountScope,
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AccountBalanceTarget {
    Login {
        id: String,
    },
    Profile {
        #[serde(rename = "profileId")]
        profile_id: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AccountImages {
    pub user: Option<String>,
    pub organization: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
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
        #[ts(optional)]
        account_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        images: Option<AccountImages>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        scope: Option<Box<AccountScope>>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct ConfidentialProfile {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
    pub name: String,
    pub provider: ServiceProvider,
    pub remote_url: String,
    pub auth: ProfileAuth,
    /// Non-secret presence metadata, independent of verification history.
    pub credential_saved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
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
                supported_endpoints: model.supported_surfaces.as_ref().map(|surfaces| {
                    surfaces
                        .iter()
                        .map(|surface| surface.path().to_string())
                        .collect()
                }),
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

#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct GatewayState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_instance: Option<String>,
    #[serde(default)]
    pub client_key_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key_available: Option<bool>,
    /// Client connection state; the backend leaves this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_connected: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_monitor_available: Option<bool>,
    /// `stopped`, `verifying` (identity and catalog not both in), `verified`,
    /// `blocked`, or `error`.
    #[ts(type = r#""stopped" | "verifying" | "verified" | "blocked" | "error""#)]
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
    /// Unix seconds when this user protection session began.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protected_since: Option<u64>,
    #[serde(default)]
    pub reconnecting: bool,
    /// User protection session, independent of the current verified transport.
    #[serde(default)]
    pub session_active: bool,
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
    /// The most recently verified catalog. A stopped gateway may retain it for
    /// agent projection and readiness state; the proxy still requires a live
    /// verified session before forwarding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogSummary>,
    #[serde(default)]
    pub web_ui: WebUiStatus,
}

/// Listener state of the service-hosted web UI. Never carries login codes or tokens.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct WebUiStatus {
    pub enabled: bool,
    #[serde(default)]
    pub listen_address: String,
    #[serde(default)]
    pub allow_network_access: bool,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_host: Option<String>,
    /// Present only while the listener is bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl From<&crate::preferences::WebUiConfig> for WebUiStatus {
    fn from(config: &crate::preferences::WebUiConfig) -> Self {
        Self {
            enabled: config.enabled,
            listen_address: config.listen_address.clone(),
            allow_network_access: config.allow_network_access,
            port: config.port,
            client_host: config.client_host.clone(),
            url: None,
            error: None,
        }
    }
}

impl GatewayState {
    pub fn is_protected(&self) -> bool {
        self.status == "verified"
            && !self.configuration_verification
            && self.api_key_saved
            && self.endpoint_error.is_none()
    }

    /// Shared by the native action label and the management client's toggle.
    pub fn should_stop_protection(&self) -> bool {
        !self.configuration_verification
            && (self.reconnecting
                || matches!(self.status.as_str(), "verifying" | "verified" | "blocked"))
    }
}

impl Default for GatewayState {
    fn default() -> Self {
        Self {
            status: "stopped".to_string(),
            backend_connected: None,
            backend_instance: None,
            client_key_revision: 0,
            client_key_available: None,
            wake_monitor_available: None,
            configuration_verification: false,
            progress: None,
            remote_url: None,
            proxy_url: None,
            endpoint_error: None,
            identity: None,
            checks: Vec::new(),
            activity: Vec::new(),
            session_id: None,
            protected_since: None,
            reconnecting: false,
            session_active: false,
            session_usage: UsageSummary::default(),
            usage_revision: 0,
            error: None,
            config: StartGatewayConfig::default(),
            profiles: Vec::new(),
            active_profile_id: String::new(),
            local_api: LocalApiConfig::default(),
            api_key_saved: false,
            catalog: None,
            web_ui: WebUiStatus::from(&crate::preferences::WebUiConfig::default()),
        }
    }
}

/// A TCP listener shared by the Local API and the web UI. Non-loopback
/// addresses require `allow_network_access`; see [`crate::listen::resolve`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct ListenConfig {
    pub listen_address: String,
    pub allow_network_access: bool,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_host: Option<String>,
}

pub type LocalApiConfig = ListenConfig;

/// The Local API listener; the web UI keeps its own defaults.
impl Default for ListenConfig {
    fn default() -> Self {
        Self {
            listen_address: "127.0.0.1".to_string(),
            allow_network_access: false,
            port: 4180,
            client_host: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, TS)]
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

#[cfg(test)]
mod typescript {
    use std::path::Path;

    use ts_rs::{Config, TS};

    use super::*;
    use crate::{
        account_login::LoginPresentation,
        agent_access::AgentAccessStatus,
        maintenance::{ImportResult, ProfileBackup, ProfileConfiguration},
        preferences::{Appearance, NotificationPreferences, UpdateChannel, WebUiConfig},
        ui_api::{LaunchPreferences, ListenAddress, Method},
        updates::{Installation, UpdateNotice},
        usage::{UsageModelPoint, UsagePage, UsagePoint, UsageQuery},
    };
    use desktop_gateway::agents::{AgentRepairAction, ConfigChange};

    const OUTPUT: &str = "src/shared/contracts.generated.ts";

    macro_rules! declarations {
        ($config:expr, $($type:ty),+ $(,)?) => {
            [$(format!(
                "{}export {}\n",
                <$type as TS>::docs().unwrap_or_default(),
                <$type as TS>::decl($config)
            )),+]
        };
    }

    fn typescript() -> String {
        let config = Config::new().with_large_int("number");
        let methods: Vec<_> = Method::ALL
            .iter()
            .map(|method| format!("\"{}\"", method.name()))
            .collect();
        let mut output = String::from(
            "// Generated from the Rust contracts by `npm run generate:contracts`. Do not edit.\n\n",
        );
        for declaration in declarations!(
            &config,
            GatewayState,
            VerificationCheck,
            GatewayIdentity,
            SourceProvenance,
            RequestActivity,
            UsageSummary,
            CatalogSummary,
            ModelSummary,
            ServiceProvider,
            ProfileAuth,
            AccountImages,
            AccountScope,
            ConfidentialProfile,
            ConfidentialProfileInput,
            AccountWorkspace,
            AccountLoginDetails,
            AccountBalance,
            AccountBalanceTarget,
            LoginPresentation,
            StartGatewayConfig,
            ListenConfig,
            WebUiConfig,
            WebUiStatus,
            ListenAddress,
            Appearance,
            UpdateChannel,
            Installation,
            UpdateNotice,
            NotificationPreferences,
            LaunchPreferences,
            ProfileBackup,
            ProfileConfiguration,
            ImportResult,
            UsageQuery,
            UsagePage,
            UsagePoint,
            UsageModelPoint,
            AgentStatus,
            AgentRepairAction,
            AgentPreview,
            ConfigChange,
            ConnectOptions,
            AgentAccessStatus,
        ) {
            output.push_str(&declaration);
        }
        output.push_str(&format!(
            "/** A method the shared UI API accepts (`ui_api::Method`). */\nexport type UiMethod = {};\n",
            methods.join(" | ")
        ));
        output
            .lines()
            .map(|line| format!("{}\n", line.trim_end()))
            .collect()
    }

    #[test]
    fn generated_typescript_contracts_are_current() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join(OUTPUT);
        let expected = typescript();
        if std::env::var_os("PAP_WRITE_CONTRACTS").is_some() {
            std::fs::write(&path, &expected).unwrap();
            return;
        }
        let current = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            current.replace("\r\n", "\n") == expected,
            "{OUTPUT} is stale; run `npm run generate:contracts` in apps/desktop"
        );
    }
}
