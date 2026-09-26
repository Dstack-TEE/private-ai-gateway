//! Shapes shared with the renderer. `src/shared/contracts.generated.ts` is
//! generated from them; run `npm run generate:contracts` after changing one.

pub use crate::agents::{AgentPreview, AgentStatus, ConnectOptions};
pub use crate::protection::{Protection, VerificationStatus};
use schemars::JsonSchema;
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
pub struct ServiceIdentity {
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

/// A question the desktop app asks before an action (`useConfirm`). macOS
/// shows it as an alert sheet on the window; elsewhere the window's own dialog
/// asks it.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(optional_fields)]
pub struct Confirmation {
    pub title: String,
    pub message: String,
    pub confirm_label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_label: Option<String>,
    /// The action deletes, revokes or resets something that can't be restored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destructive: Option<bool>,
}

/// One request seen by the local proxy: forwarded through the verifier (with
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ServiceProvider {
    Phala,
    Redpill,
    Custom,
}

impl ServiceProvider {
    pub const ALL: [Self; 3] = [Self::Phala, Self::Redpill, Self::Custom];
    /// The provider a new profile and a fresh installation start with.
    pub const DEFAULT: Self = Self::Redpill;

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

    pub const fn key_label(self) -> &'static str {
        match self {
            Self::Phala => "Phala API key",
            Self::Redpill => "RedPill API key",
            Self::Custom => "API key",
        }
    }

    /// Whether a profile can sign in with an account instead of an API key.
    pub const fn account_login(self) -> bool {
        !matches!(self, Self::Custom)
    }

    /// Whether an account signs in to one of its workspaces.
    pub const fn workspaces(self) -> bool {
        matches!(self, Self::Redpill)
    }

    /// Whether a sign-in returns to a loopback callback URL the user can also
    /// paste; Phala signs in with a device code instead.
    pub const fn callback_url(self) -> bool {
        matches!(self, Self::Redpill)
    }
}

/// A provider as the profile editor presents it.
#[derive(Clone, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct ServiceProviderInfo {
    pub id: ServiceProvider,
    pub label: &'static str,
    pub preset_url: Option<&'static str>,
    pub key_label: &'static str,
    pub account_login: bool,
    pub workspaces: bool,
    pub callback_url: bool,
}

impl From<ServiceProvider> for ServiceProviderInfo {
    fn from(provider: ServiceProvider) -> Self {
        Self {
            id: provider,
            label: provider.label(),
            preset_url: provider.preset_url(),
            key_label: provider.key_label(),
            account_login: provider.account_login(),
            workspaces: provider.workspaces(),
            callback_url: provider.callback_url(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase")]
pub enum AccountSaveResult {
    Running,
    Complete { state: Box<AppStateWire> },
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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS, JsonSchema)]
pub struct AccountImages {
    pub user: Option<String>,
    pub organization: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind")]
pub enum ProfileAuth {
    #[default]
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

impl ProfileAuth {
    pub fn is_api_key(&self) -> bool {
        matches!(self, Self::ApiKey)
    }
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

/// The backend's state. Clients receive it as [`AppStateWire`].
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct AppState {
    /// The backend process whose state this is (its `/api/version` instance).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_instance: Option<String>,
    /// Increases with every state the backend instance publishes, so a client
    /// that receives states from events and command results keeps the newest.
    #[serde(default)]
    pub sequence: u64,
    #[serde(default)]
    pub client_key_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_key_available: Option<bool>,
    /// Client connection state; the backend leaves this unset. `false`
    /// without an `error` while the client is still starting the backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend_connected: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_monitor_available: Option<bool>,
    pub status: VerificationStatus,
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
    pub identity: Option<ServiceIdentity>,
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
    pub config: StartConfig,
    pub profiles: Vec<ConfidentialProfile>,
    pub active_profile_id: String,
    pub local_api: ListenConfig,
    pub api_key_saved: bool,
    /// The most recently verified catalog. A stopped gateway may retain it for
    /// agent projection and readiness state; the proxy still requires a live
    /// verified session before forwarding.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub catalog: Option<CatalogSummary>,
    #[serde(default)]
    pub web_ui: WebUiStatus,
    #[serde(default)]
    pub config_files: ConfigFiles,
    /// Changes whenever the agents the backend reports change.
    #[serde(default)]
    pub agents_revision: u64,
}

/// `AppState` as the management API answers and publishes it: the state and
/// the [`Protection`] it presents. Every state-returning command answers one
/// and every state event carries one, each built from its state with `From`,
/// so none can carry a presentation of another state.
#[derive(Clone, Debug, Deserialize, Serialize, TS)]
#[ts(rename = "AppState")]
pub struct AppStateWire {
    #[serde(flatten)]
    pub state: AppState,
    pub protection: Protection,
}

impl From<AppState> for AppStateWire {
    fn from(state: AppState) -> Self {
        let protection = state.protection();
        Self { state, protection }
    }
}

/// The settings files the backend reads. Never carries their contents.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct ConfigFiles {
    pub config_path: String,
    pub credentials_path: String,
    /// Why the current file contents are not in effect (the previous settings
    /// stay in effect), with the file, line and column.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Problems that do not stop the files from applying: unknown keys, which
    /// are ignored, and what the 0.1 import could not bring over.
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Changes whenever applied settings change, including external edits.
    pub revision: u64,
}

/// Listener state of the service-hosted web UI. Never carries the password, its hash or session tokens.
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

impl From<&crate::config::WebUiConfig> for WebUiStatus {
    fn from(config: &crate::config::WebUiConfig) -> Self {
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

impl AppState {
    pub fn protection(&self) -> Protection {
        Protection::of(self)
    }

    /// The state a client shows once the backend stopped answering.
    pub fn disconnect(&mut self, error: String) {
        self.status = VerificationStatus::Error;
        self.backend_connected = Some(false);
        self.identity = None;
        self.proxy_url = None;
        self.error = Some(error);
        // Every desktop distribution, the App Store one included, offers to
        // start the service again; not every one has the `pap` command.
        self.endpoint_error = Some("The background service stopped.".into());
    }

    pub fn is_protected(&self) -> bool {
        self.status == VerificationStatus::Verified
            && !self.configuration_verification
            && self.api_key_saved
            && self.endpoint_error.is_none()
    }

    /// Shared by the native action label and the management client's toggle.
    pub fn should_stop_protection(&self) -> bool {
        !self.configuration_verification
            && (self.reconnecting
                || matches!(
                    self.status,
                    VerificationStatus::Verifying
                        | VerificationStatus::Verified
                        | VerificationStatus::Blocked
                ))
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            status: VerificationStatus::Stopped,
            backend_connected: None,
            backend_instance: None,
            sequence: 0,
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
            config: StartConfig::default(),
            profiles: Vec::new(),
            active_profile_id: String::new(),
            local_api: ListenConfig::default(),
            api_key_saved: false,
            catalog: None,
            web_ui: WebUiStatus::from(&crate::config::WebUiConfig::default()),
            config_files: ConfigFiles::default(),
            agents_revision: 0,
        }
    }
}

/// A TCP listener shared by the Local API and the web UI. Non-loopback
/// addresses require `allow_network_access`; see [`crate::listen::resolve`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
#[ts(optional_fields)]
pub struct ListenConfig {
    pub listen_address: String,
    pub allow_network_access: bool,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_host: Option<String>,
}

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct StartConfig {
    pub remote_url: String,
    pub require_production_os: bool,
}

impl Default for StartConfig {
    fn default() -> Self {
        Self {
            remote_url: crate::brand::SERVICE_DEFAULT_URL.to_string(),
            require_production_os: true,
        }
    }
}

/// A `pap cli status|install|uninstall` result; the desktop shell reads it
/// from the CLI's JSON output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CommandRegistration {
    pub executable: std::path::PathBuf,
    pub command_path: std::path::PathBuf,
    pub installed: bool,
    pub on_path: bool,
}

/// The `pap` command registration the renderer shows, with the error from
/// the desktop app's automatic registration attempt, if any.
#[derive(Debug, Clone, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct CliRegistration {
    #[serde(flatten)]
    pub registration: CommandRegistration,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup_error: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum DistributionChannel {
    Direct,
    MacAppStore,
    Web,
}

/// What this distribution of the app may offer; the renderer hides the rest.
#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct DistributionCapabilities {
    pub channel: DistributionChannel,
    pub native_updates: bool,
    pub cli_registration: bool,
    pub account_portal_links: bool,
    pub sandbox_home_access: bool,
    pub launch_at_login: bool,
    pub notifications: bool,
    pub web_ui: bool,
}

impl DistributionCapabilities {
    /// The web UI's, which the renderer receives as a generated constant: the
    /// backend's own installation owns updates and the browser has no OS
    /// integration.
    pub const WEB: Self = Self {
        channel: DistributionChannel::Web,
        native_updates: false,
        cli_registration: false,
        account_portal_links: true,
        sandbox_home_access: false,
        launch_at_login: false,
        notifications: false,
        web_ui: true,
    };
}

/// What a tray or menu item asks the main window to show or open.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum NavigationTarget {
    Settings,
    Agents,
    Profiles,
    ProfileSetup,
}

/// The system's permission to show this app's notifications.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(rename_all = "camelCase")]
pub enum NotificationPermission {
    Granted,
    Denied,
    NotDetermined,
    Unknown,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Serialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(optional_fields)]
pub struct NotificationPermissionStatus {
    pub permission: NotificationPermission,
    /// Whether banner alerts are on, where the system reports it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alerts_enabled: Option<bool>,
}

impl From<NotificationPermission> for NotificationPermissionStatus {
    fn from(permission: NotificationPermission) -> Self {
        Self {
            permission,
            alerts_enabled: None,
        }
    }
}

/// The notification preferences and the system permission they need.
#[derive(Clone, Copy, Debug, Serialize, TS)]
pub struct NotificationConfiguration {
    pub preferences: crate::config::NotificationPreferences,
    #[serde(flatten)]
    pub system: NotificationPermissionStatus,
}

/// `GET /api/bootstrap` on the web UI; it also tells whether this browser has
/// a session.
#[derive(Clone, Debug, Serialize, TS)]
pub struct WebBootstrap {
    pub version: String,
}

#[cfg(test)]
mod typescript {
    use std::path::Path;

    use ts_rs::{Config, TS};

    use super::*;
    use crate::{
        account::{api_key_page, LoginPresentation},
        agent_access::AgentAccessStatus,
        agents::{Agent, AgentRepairAction, ConfigChange},
        brand::AboutLink,
        config::{
            Appearance, NotificationPreferences, UpdateChannel, WebUiConfig,
            WEB_UI_PASSWORD_MIN_LENGTH,
        },
        maintenance::{ImportResult, ProfileBackup, ProfileConfiguration},
        protection::{ProtectionAction, ProtectionOperation, ProtectionPhase, Tone},
        ui_api::{self, LaunchPreference, LaunchPreferences, ListenAddress, Method},
        updates::{Installation, UpdateInfo, UpdateNotice},
        usage::{UsageModelPoint, UsagePage, UsagePoint, UsageQuery},
    };

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
            AppStateWire,
            VerificationStatus,
            Protection,
            ProtectionPhase,
            ProtectionAction,
            ProtectionOperation,
            Tone,
            VerificationCheck,
            ServiceIdentity,
            SourceProvenance,
            RequestActivity,
            UsageSummary,
            CatalogSummary,
            ModelSummary,
            ServiceProvider,
            ServiceProviderInfo,
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
            StartConfig,
            ListenConfig,
            WebUiConfig,
            WebUiStatus,
            ConfigFiles,
            ListenAddress,
            Appearance,
            UpdateChannel,
            Installation,
            UpdateNotice,
            NotificationPreferences,
            LaunchPreferences,
            LaunchPreference,
            NotificationPermission,
            NotificationPermissionStatus,
            NotificationConfiguration,
            NavigationTarget,
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
            UpdateInfo,
            CommandRegistration,
            CliRegistration,
            DistributionChannel,
            DistributionCapabilities,
            WebBootstrap,
            AboutLink,
            Confirmation,
        ) {
            output.push_str(&declaration);
        }
        output.push_str(&format!(
            "/** A method the shared UI API accepts (`ui_api::Method`). */\nexport type UiMethod = {};\n",
            methods.join(" | ")
        ));
        // Each event with the type of the payload it carries.
        let events = [
            (
                "APPEARANCE_EVENT",
                ui_api::APPEARANCE_EVENT,
                Appearance::name(&config),
            ),
            (
                "LAUNCH_PREFERENCES_EVENT",
                ui_api::LAUNCH_PREFERENCES_EVENT,
                LaunchPreferences::name(&config),
            ),
            (
                "SETTINGS_RESET_EVENT",
                ui_api::SETTINGS_RESET_EVENT,
                <()>::name(&config),
            ),
            (
                "STATE_EVENT",
                ui_api::STATE_EVENT,
                AppStateWire::name(&config),
            ),
            (
                "CLIENT_KEY_CHANGED_EVENT",
                ui_api::CLIENT_KEY_CHANGED_EVENT,
                bool::name(&config),
            ),
            (
                "AGENTS_CHANGED_EVENT",
                ui_api::AGENTS_CHANGED_EVENT,
                <()>::name(&config),
            ),
            (
                "NAVIGATE_EVENT",
                ui_api::NAVIGATE_EVENT,
                NavigationTarget::name(&config),
            ),
            (
                "CONFIRM_STOP_ALL_EVENT",
                ui_api::CONFIRM_STOP_ALL_EVENT,
                <()>::name(&config),
            ),
        ];
        for (name, value, _) in &events {
            output.push_str(&format!(
                "export const {name} = {};\n",
                serde_json::json!(value)
            ));
        }
        let payloads: Vec<_> = events
            .iter()
            .map(|(_, event, payload)| format!("{}: {payload}", serde_json::json!(event)))
            .collect();
        output.push_str(&format!(
            "/** The payload each event carries. */\nexport type UiEventPayloads = {{ {} }};\nexport type UiEvent = keyof UiEventPayloads;\n",
            payloads.join(", ")
        ));
        let links: serde_json::Map<_, _> = AboutLink::ALL
            .into_iter()
            .map(|link| (name(&link), link.url().into()))
            .collect();
        output.push_str(&constant(
            "ABOUT_LINKS",
            "Record<AboutLink, string>",
            links.into(),
        ));
        let agents: Vec<_> = Agent::ALL
            .into_iter()
            .map(|agent| {
                serde_json::json!({ "id": agent.id(), "name": agent.name(), "website": agent.website() })
            })
            .collect();
        output.push_str(&constant(
            "AGENTS",
            "ReadonlyArray<{ id: string, name: string, website: string }>",
            agents.into(),
        ));
        let api_key_pages: serde_json::Map<_, _> = ServiceProvider::ALL
            .into_iter()
            .filter_map(|provider| Some((name(&provider), api_key_page(provider)?.into())))
            .collect();
        output.push_str(&constant(
            "API_KEY_PAGES",
            "Partial<Record<ServiceProvider, string>>",
            api_key_pages.into(),
        ));
        let providers: serde_json::Map<_, _> = ServiceProvider::ALL
            .into_iter()
            .map(|provider| {
                let info = ServiceProviderInfo::from(provider);
                (name(&provider), serde_json::to_value(info).unwrap())
            })
            .collect();
        output.push_str(&constant(
            "SERVICE_PROVIDERS",
            "Readonly<Record<ServiceProvider, ServiceProviderInfo>>",
            providers.into(),
        ));
        output.push_str(&constant(
            "DEFAULT_SERVICE_PROVIDER",
            "ServiceProvider",
            serde_json::to_value(ServiceProvider::DEFAULT).unwrap(),
        ));
        output.push_str(&constant("BYLINE", "string", crate::brand::BYLINE.into()));
        output.push_str(&constant(
            "INITIAL_STATE",
            "AppState",
            serde_json::to_value(AppStateWire::from(AppState::default())).unwrap(),
        ));
        let mut unavailable = AppState::default();
        unavailable.disconnect("The background service is unavailable.".into());
        output.push_str(&constant(
            "UNAVAILABLE_STATE",
            "AppState",
            serde_json::to_value(AppStateWire::from(unavailable)).unwrap(),
        ));
        output.push_str(&constant(
            "WEB_UI_PASSWORD_MIN_LENGTH",
            "number",
            WEB_UI_PASSWORD_MIN_LENGTH.into(),
        ));
        output.push_str(&constant(
            "WEB_DISTRIBUTION",
            "DistributionCapabilities",
            serde_json::to_value(DistributionCapabilities::WEB).unwrap(),
        ));
        output.push_str(&constant(
            "DEFAULT_LOCAL_API_CONFIG",
            "ListenConfig",
            serde_json::to_value(ListenConfig::default()).unwrap(),
        ));
        output.push_str(&constant(
            "DEFAULT_WEB_UI_CONFIG",
            "WebUiConfig",
            serde_json::to_value(WebUiConfig::default()).unwrap(),
        ));
        output
            .lines()
            .map(|line| format!("{}\n", line.trim_end()))
            .collect()
    }

    /// A value the Rust side owns, as a typed TypeScript constant.
    fn constant(name: &str, ty: &str, value: serde_json::Value) -> String {
        format!("export const {name}: {ty} = {value};\n")
    }

    /// The serialized name of a unit enum variant.
    fn name(value: &impl Serialize) -> String {
        serde_json::to_value(value)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A wire state presents exactly the state it carries.
    #[test]
    fn a_wire_state_carries_the_protection_it_presents() {
        let mut state = AppState {
            status: VerificationStatus::Verified,
            api_key_saved: true,
            ..AppState::default()
        };
        let changes: [fn(&mut AppState); 4] = [
            |_| {},
            |state| state.status = VerificationStatus::Stopped,
            |state| state.endpoint_error = Some("Port in use".into()),
            |state| state.disconnect("Connection closed".into()),
        ];
        for change in changes {
            change(&mut state);
            let serialized = serde_json::to_value(AppStateWire::from(state.clone())).unwrap();
            assert_eq!(
                serialized["status"],
                serde_json::to_value(state.status).unwrap()
            );
            assert_eq!(
                serialized["protection"],
                serde_json::to_value(state.protection()).unwrap()
            );
        }
    }
}
