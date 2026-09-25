//! The user settings file, `config.toml`, in the settings directory
//! ([`crate::paths::config_dir`]): profiles, the Local API and web UI
//! listeners, and preferences. It never holds a secret; those live in
//! `credentials.toml` beside it, which only the backend reads.
//!
//! The layout follows Cargo's and Codex's `config.toml`: every key is
//! optional, `[profiles.<id>]` tables name the saved profiles and
//! `active-profile` selects one. Keys are kebab-case, as in `Cargo.toml`,
//! Cargo's `config.toml`, `Tauri.toml` and Helix's `config.toml`; the
//! management contracts keep their camelCase JSON names, so the nested
//! contract types are (de)serialized here through serde remote definitions
//! (<https://serde.rs/remote-derive.html>). Unknown keys are ignored and reported, as Cargo reports an
//! "unused config key", so a file written by a newer version still loads.
//! Clients only read this file; the backend applies external edits and
//! performs every write (see `desktop_runtime::settings`).

use std::{fs, path::PathBuf};

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{de::DeserializeOwned, Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

use crate::{
    contracts::{
        AccountImages, AccountScope, ConfidentialProfile, ConfidentialProfileInput, ListenConfig,
        ProfileAuth, ServiceProvider, StartConfig,
    },
    listen::{self, ResolvedListen},
    paths::config_dir,
};

pub const CONFIG_FILE: &str = "config.toml";
pub const CREDENTIALS_FILE: &str = "credentials.toml";
/// The JSON Schema for `config.toml`, written beside it for editors.
pub const SCHEMA_FILE: &str = "config.schema.json";
/// Clear of the Local API (4180) and the account callback (4181).
pub const WEB_UI_DEFAULT_PORT: u16 = 4182;
/// The shortest web UI password, in characters (NIST SP 800-63B).
pub const WEB_UI_PASSWORD_MIN_LENGTH: usize = 12;
const MAX_PROFILES: usize = 50;
const MAX_KEY_LEN: usize = 512;

/// The header of a new `config.toml`. The `#:schema` directive is how taplo
/// (Even Better TOML and other editors) find the schema written beside it.
pub const CONFIG_HEADER: &str = "#:schema ./config.schema.json
# Private AI Proxy settings. Saved edits apply immediately; an invalid edit is
# reported and the previous settings stay in effect. The app and
# `pap settings set` edit this file in place and keep your comments.
# Secrets never go here: API keys and the web UI password live in
# credentials.toml. Reference: `pap settings schema`.
";

/// Private AI Proxy settings (`config.toml`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, rename_all = "kebab-case")]
pub struct Config {
    /// The profile protection uses. Defaults to the first profile.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub active_profile: String,
    /// Refuse services whose attestation reports a development OS image.
    pub require_production_os: bool,
    /// Protect on launch: start protection when the backend starts.
    pub connect_on_launch: bool,
    pub appearance: Appearance,
    /// Release channel for update checks. Defaults to the channel of the running build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_channel: Option<UpdateChannel>,
    /// Register the `pap` command when the desktop app starts (macOS). Defaults to true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_cli_registration: Option<bool>,
    #[serde(with = "NotificationsTable")]
    pub notifications: NotificationPreferences,
    /// The machine-local inference API that agents use.
    #[serde(with = "ListenTable")]
    pub local_api: ListenConfig,
    /// The browser UI hosted by the backend.
    #[serde(with = "WebUiTable")]
    pub web_ui: WebUiConfig,
    /// Confidential AI service profiles by ID. Their API keys are in credentials.toml.
    #[serde(skip_serializing_if = "IndexMap::is_empty")]
    pub profiles: IndexMap<String, Profile>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            active_profile: String::new(),
            require_production_os: true,
            connect_on_launch: false,
            appearance: Appearance::default(),
            update_channel: None,
            auto_cli_registration: None,
            notifications: NotificationPreferences::default(),
            local_api: ListenConfig::default(),
            web_ui: WebUiConfig::default(),
            profiles: IndexMap::new(),
        }
    }
}

/// A saved Confidential AI service profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub struct Profile {
    pub name: String,
    pub provider: ServiceProvider,
    /// The service's HTTPS URL (HTTP only for loopback development).
    pub remote_url: String,
    /// How the credential was obtained. Defaults to a manually entered API key.
    #[serde(
        default,
        skip_serializing_if = "ProfileAuth::is_api_key",
        with = "ProfileAuthTable"
    )]
    pub auth: ProfileAuth,
    /// Unix seconds of the last successful verification from Settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    Beta,
    Stable,
}

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ts_rs::TS,
)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

/// Desktop notifications. The OS permission is managed by the desktop app.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct NotificationPreferences {
    pub enabled: bool,
    pub gateway: bool,
    pub local_api: bool,
    pub verification: bool,
}

impl Default for NotificationPreferences {
    fn default() -> Self {
        Self {
            enabled: true,
            gateway: true,
            local_api: true,
            verification: true,
        }
    }
}

/// The service-hosted browser UI. It is off until the user enables it and
/// listens on loopback unless network access is explicitly allowed. It
/// cannot turn on without a sign-in password (`pap settings set web-ui.password`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
#[ts(optional_fields)]
pub struct WebUiConfig {
    pub enabled: bool,
    pub listen_address: String,
    pub allow_network_access: bool,
    pub port: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_host: Option<String>,
}

impl WebUiConfig {
    pub fn listen(&self) -> ListenConfig {
        ListenConfig {
            listen_address: self.listen_address.clone(),
            allow_network_access: self.allow_network_access,
            port: self.port,
            client_host: self.client_host.clone(),
        }
    }
}

impl Default for WebUiConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            listen_address: "127.0.0.1".into(),
            allow_network_access: false,
            port: WEB_UI_DEFAULT_PORT,
            client_host: None,
        }
    }
}

// The file's names for the management contract types it embeds. Each mirrors
// its remote type field for field and only renames the keys; unknown keys are
// allowed, unlike in the contracts. The compiler checks the field names and
// types against the remote type, but not the serde attributes (`default`,
// `skip_serializing_if`): keep those in step with the contract type by hand.
// `every_setting_survives_a_round_trip_through_the_file` catches drift that
// loses a value.

/// Desktop notifications. The OS permission is managed by the desktop app.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(
    remote = "NotificationPreferences",
    default = "NotificationPreferences::default",
    rename_all = "kebab-case"
)]
#[schemars(rename = "Notifications")]
struct NotificationsTable {
    enabled: bool,
    gateway: bool,
    local_api: bool,
    verification: bool,
}

/// A TCP listener. Non-loopback addresses require `allow-network-access`.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(
    remote = "ListenConfig",
    default = "ListenConfig::default",
    rename_all = "kebab-case"
)]
#[schemars(rename = "Listener")]
struct ListenTable {
    listen_address: String,
    allow_network_access: bool,
    port: u16,
    /// The host name clients use when listening on every interface.
    #[serde(skip_serializing_if = "Option::is_none")]
    client_host: Option<String>,
}

/// The service-hosted browser UI. It is off until the user enables it and
/// listens on loopback unless network access is explicitly allowed. It
/// cannot turn on without a sign-in password (`pap settings set web-ui.password`).
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(
    remote = "WebUiConfig",
    default = "WebUiConfig::default",
    rename_all = "kebab-case"
)]
#[schemars(rename = "WebUi")]
struct WebUiTable {
    enabled: bool,
    listen_address: String,
    allow_network_access: bool,
    port: u16,
    /// The host name clients use when listening on every interface.
    #[serde(skip_serializing_if = "Option::is_none")]
    client_host: Option<String>,
}

/// How a profile's credential was obtained.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(remote = "ProfileAuth", tag = "kind", rename_all = "kebab-case")]
#[schemars(rename = "ProfileAuth")]
enum ProfileAuthTable {
    /// An API key entered manually.
    ApiKey,
    /// A key created by signing in to the provider account.
    #[serde(rename = "oauth", rename_all = "kebab-case")]
    OAuth {
        account_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        account_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        images: Option<AccountImages>,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "AccountScopeTable"
        )]
        scope: Option<Box<AccountScope>>,
    },
}

/// The account organization and workspace the key belongs to.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(remote = "AccountScope", rename_all = "kebab-case")]
#[schemars(rename = "AccountScope")]
struct AccountScopeFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    organization_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    organization_slug: Option<String>,
    organization: Option<String>,
    workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_slug: Option<String>,
    workspace_id: Option<i64>,
}

// `Option<Box<AccountScope>>` through `AccountScopeFields`: the wrapper
// serde's remote derive needs for a remote type inside a container.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
struct AccountScopeTable(#[serde(with = "AccountScopeFields")] AccountScope);

impl AccountScopeTable {
    fn serialize<S: Serializer>(
        scope: &Option<Box<AccountScope>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        scope
            .as_deref()
            .map(|scope| Self(scope.clone()))
            .serialize(serializer)
    }

    fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Box<AccountScope>>, D::Error> {
        Ok(Option::<Self>::deserialize(deserializer)?.map(|scope| Box::new(scope.0)))
    }
}

impl Config {
    pub fn active(&self) -> Option<(&String, &Profile)> {
        self.profiles.get_key_value(&self.active_profile)
    }

    pub fn runtime_config(&self) -> StartConfig {
        StartConfig {
            remote_url: self.active().map_or_else(
                || crate::brand::SERVICE_DEFAULT_URL.to_string(),
                |(_, profile)| profile.remote_url.clone(),
            ),
            require_production_os: self.require_production_os,
        }
    }

    /// The profiles as management clients see them. `credential` returns the
    /// saved credential's identity for a profile ID, if one is saved.
    pub fn profile_views(
        &self,
        credential: impl Fn(&str) -> Option<String>,
    ) -> Vec<ConfidentialProfile> {
        self.profiles
            .iter()
            .map(|(id, profile)| {
                let credential_ref = credential(id);
                ConfidentialProfile {
                    id: id.clone(),
                    credential_saved: credential_ref.is_some(),
                    credential_ref,
                    name: profile.name.clone(),
                    provider: profile.provider,
                    remote_url: profile.remote_url.clone(),
                    auth: profile.auth.clone(),
                    verified_at: profile.verified_at,
                }
            })
            .collect()
    }

    /// Adds or replaces a profile, keeping its position.
    pub fn upsert(&mut self, id: String, profile: Profile) -> Result<(), String> {
        if !self.profiles.contains_key(&id) && self.profiles.len() >= MAX_PROFILES {
            return Err(format!(
                "At most {MAX_PROFILES} Confidential AI profiles are allowed"
            ));
        }
        self.profiles.insert(id, profile);
        Ok(())
    }
}

/// Reads and validates `config.toml`; a missing file is the default settings.
pub fn load() -> Result<Config, String> {
    let path = config_path()?;
    match fs::read_to_string(&path) {
        Ok(text) => parse(&text).map(|parsed| parsed.value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(format!("Cannot read {}: {error}", path.display())),
    }
}

pub fn config_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join(CONFIG_FILE))
}

pub fn credentials_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join(CREDENTIALS_FILE))
}

/// A settings file that loaded, with a warning for each key it does not know.
#[derive(Debug, Default)]
pub struct Parsed<T> {
    pub value: T,
    /// `file:line:column: key.path: unknown key, ignored`.
    pub unknown: Vec<String>,
}

/// Parses and validates `config.toml` text. Errors name the file, line and
/// column: `config.toml:3:8: invalid type: string "x", expected u16`.
pub fn parse(text: &str) -> Result<Parsed<Config>, String> {
    let mut parsed: Parsed<Config> = parse_toml(CONFIG_FILE, text, true)?;
    validate(&mut parsed.value).map_err(|invalid| invalid.located(CONFIG_FILE, text))?;
    Ok(parsed)
}

/// Deserializes a settings file, reporting syntax and type errors with their
/// position. `describe` includes the parser's message, which can quote a
/// value; files holding secrets leave it out. A key the file's type does not
/// know is skipped and reported instead of failing the file, like Cargo's
/// "unused config key" warning (collected with `serde_ignored`, as Cargo does).
pub fn parse_toml<T: DeserializeOwned>(
    file: &str,
    text: &str,
    describe: bool,
) -> Result<Parsed<T>, String> {
    let located = |error: toml_edit::de::Error| {
        let message = if describe {
            error.message().trim_end().to_string()
        } else {
            "invalid entry".to_string()
        };
        match error.span() {
            Some(span) => {
                let (line, column) = line_column(text, span.start);
                format!("{file}:{line}:{column}: {message}")
            }
            None => format!("{file}: {message}"),
        }
    };
    let deserializer = toml_edit::de::Deserializer::parse(text).map_err(located)?;
    let mut unknown = Vec::new();
    let value = serde_ignored::deserialize(deserializer, |path| {
        let mut keys = Vec::new();
        key_path(&path, &mut keys);
        unknown.push(keys);
    })
    .map_err(located)?;
    let unknown = unknown
        .into_iter()
        .map(|path| {
            Invalid {
                path,
                message: "unknown key, ignored".into(),
            }
            .located(file, text)
        })
        .collect();
    Ok(Parsed { value, unknown })
}

fn key_path(path: &serde_ignored::Path<'_>, keys: &mut Vec<String>) {
    use serde_ignored::Path;
    match path {
        Path::Root => {}
        Path::Seq { parent, index } => {
            key_path(parent, keys);
            keys.push(index.to_string());
        }
        Path::Map { parent, key } => {
            key_path(parent, keys);
            keys.push(key.clone());
        }
        Path::Some { parent }
        | Path::NewtypeStruct { parent }
        | Path::NewtypeVariant { parent } => key_path(parent, keys),
    }
}

/// A setting that parsed but is not allowed, at its key path.
pub struct Invalid {
    pub path: Vec<String>,
    pub message: String,
}

impl Invalid {
    fn at(path: &[&str], message: impl Into<String>) -> Self {
        Self {
            path: path.iter().map(|part| part.to_string()).collect(),
            message: message.into(),
        }
    }

    /// `file:line:column: key.path: message`, positioned at the deepest key present.
    pub fn located(self, file: &str, text: &str) -> String {
        let key = self.path.join(".");
        let document = toml_edit::Document::parse(text).ok();
        let span = document.as_ref().and_then(|document| {
            let mut item = document.as_item();
            let mut span = None;
            for part in &self.path {
                match item.get(part.as_str()) {
                    Some(next) => {
                        item = next;
                        span = next.span().or(span);
                    }
                    None => break,
                }
            }
            span
        });
        match span {
            Some(span) => {
                let (line, column) = line_column(text, span.start);
                format!("{file}:{line}:{column}: {key}: {}", self.message)
            }
            None => format!("{file}: {key}: {}", self.message),
        }
    }
}

/// The 1-based line and column of a byte offset.
pub fn line_column(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let column = before
        .rsplit('\n')
        .next()
        .map_or(0, |line| line.chars().count())
        + 1;
    (line, column)
}

/// Checks and normalizes the settings the way the management commands do.
pub fn validate(config: &mut Config) -> Result<(), Invalid> {
    if config.profiles.len() > MAX_PROFILES {
        return Err(Invalid::at(
            &["profiles"],
            format!("At most {MAX_PROFILES} profiles are allowed"),
        ));
    }
    for (id, profile) in &mut config.profiles {
        let resolved = resolve_profile(
            ConfidentialProfileInput {
                id: id.clone(),
                name: profile.name.clone(),
                provider: profile.provider,
                remote_url: profile.remote_url.clone(),
            },
            profile.verified_at,
        )
        .map_err(|message| {
            let field = if message.starts_with("Profile ID") {
                None
            } else if message.starts_with("Profile name") {
                Some("name")
            } else {
                Some("remote-url")
            };
            let mut path = vec!["profiles", id.as_str()];
            path.extend(field);
            Invalid::at(&path, message)
        })?;
        if let ProfileAuth::OAuth { account_id, .. } = &profile.auth {
            if account_id.trim().is_empty() {
                return Err(Invalid::at(
                    &["profiles", id, "auth", "account-id"],
                    "OAuth profiles must identify an account",
                ));
            }
        }
        profile.name = resolved.name;
        profile.remote_url = resolved.remote_url;
    }
    if config.active_profile.is_empty() {
        config.active_profile = config.profiles.keys().next().cloned().unwrap_or_default();
    } else if !config.profiles.contains_key(&config.active_profile) {
        return Err(Invalid::at(
            &["active-profile"],
            "The active profile does not exist",
        ));
    }
    config.local_api = resolve_local_api(config.local_api.clone())
        .map_err(|message| Invalid::at(&["local-api"], message))?
        .config;
    let web_ui = validate_web_ui(&config.web_ui, config.local_api.port)
        .map_err(|message| Invalid::at(&["web-ui"], message))?;
    config.web_ui.listen_address = web_ui.config.listen_address;
    config.web_ui.client_host = web_ui.config.client_host;
    Ok(())
}

/// The Local API port range policy plus the shared listener rules.
pub fn resolve_local_api(config: ListenConfig) -> Result<ResolvedListen, String> {
    if config.port < 1024 {
        return Err("Port must be between 1024 and 65535".to_string());
    }
    listen::resolve(config)
}

/// Checks the port policy and the shared listener rules; non-loopback fails closed.
/// Unlike the Local API, privileged ports are allowed: agents never store this
/// port, and a failed bind only disables the optional web UI (see docs/cli.md).
pub fn validate_web_ui(
    config: &WebUiConfig,
    local_api_port: u16,
) -> Result<ResolvedListen, String> {
    let port = config.port;
    if port == 0 {
        return Err("Web UI port must be between 1 and 65535".into());
    }
    if port == local_api_port {
        return Err(format!(
            "Web UI port {port} is used by the Local API; choose another port"
        ));
    }
    if port == crate::account::CALLBACK_PORT {
        return Err(format!(
            "Web UI port {port} is reserved for account connection callbacks; choose another port"
        ));
    }
    listen::resolve(config.listen()).map_err(|error| format!("Web UI: {error}"))
}

pub fn resolve_profile(
    input: ConfidentialProfileInput,
    verified_at: Option<u64>,
) -> Result<ConfidentialProfile, String> {
    validate_profile_id(&input.id)?;
    let name = input.name.trim();
    if name.is_empty() || name.len() > 80 || name.chars().any(char::is_control) {
        return Err("Profile name must be between 1 and 80 characters".to_string());
    }
    let remote_url = normalize_url(&input.remote_url)?;
    if let Some(expected) = input.provider.preset_url() {
        if remote_url != expected {
            return Err(format!(
                "The {} preset must use {expected}",
                input.provider.label()
            ));
        }
    }
    Ok(ConfidentialProfile {
        id: input.id,
        credential_ref: None,
        name: name.to_string(),
        provider: input.provider,
        remote_url,
        auth: ProfileAuth::ApiKey,
        credential_saved: false,
        verified_at,
    })
}

pub fn resolve_runtime_config(mut config: StartConfig) -> Result<StartConfig, String> {
    config.remote_url = normalize_url(&config.remote_url)?;
    Ok(config)
}

/// Validate a key the user typed: trimmed, single line, bounded length.
pub fn validate_api_key(value: &str) -> Result<String, String> {
    let key = value.trim();
    if key.is_empty() {
        return Err("Enter an API key".to_string());
    }
    if key.len() > MAX_KEY_LEN || key.chars().any(char::is_whitespace) {
        return Err("The API key must be a single token without spaces".to_string());
    }
    Ok(key.to_string())
}

pub fn validate_profile_id(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(
            "Profile ID must contain only letters, numbers, hyphens, or underscores".to_string(),
        );
    }
    Ok(())
}

/// A service URL in the form profiles store: HTTPS (HTTP only for loopback),
/// no credentials, query or trailing slash.
pub fn normalize_url(value: &str) -> Result<String, String> {
    let mut url = Url::parse(value.trim())
        .map_err(|_| "Gateway URL must be a valid HTTP or HTTPS URL".to_string())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(
            "Gateway URL must use HTTPS (HTTP is allowed only for loopback development)"
                .to_string(),
        );
    }
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    };
    if url.scheme() != "https" && !loopback {
        return Err(
            "Gateway URL must use HTTPS unless it points to localhost or a loopback address"
                .to_string(),
        );
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Gateway URL must not contain credentials".to_string());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("Gateway URL must not contain a query or fragment".to_string());
    }
    let path = url.path().trim_end_matches('/').to_string();
    url.set_path(&path);
    Ok(url.to_string().trim_end_matches('/').to_string())
}

/// The JSON Schema of `config.toml`, generated from [`Config`].
pub fn schema() -> String {
    let mut schema = serde_json::to_string_pretty(&schemars::schema_for!(Config))
        .unwrap_or_else(|_| "{}".to_string());
    schema.push('\n');
    schema
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_and_multiline_keys() {
        assert!(validate_api_key("  ").is_err());
        assert!(validate_api_key("sk-a\nsk-b").is_err());
        assert_eq!(validate_api_key("  sk-abc  ").unwrap(), "sk-abc");
    }

    fn input(remote_url: &str) -> ConfidentialProfileInput {
        ConfidentialProfileInput {
            id: "work-profile".to_string(),
            name: "Work".to_string(),
            provider: ServiceProvider::Custom,
            remote_url: remote_url.to_string(),
        }
    }

    #[test]
    fn normalizes_remote_url_and_rejects_credentials() {
        assert_eq!(
            resolve_profile(input(" https://private.example.com/ "), None)
                .unwrap()
                .remote_url,
            "https://private.example.com"
        );
        assert!(resolve_profile(input("https://token@private.example.com"), None).is_err());
        assert!(resolve_profile(input("file:///tmp/gateway"), None).is_err());
        assert!(resolve_profile(input("http://private.example.com"), None).is_err());
        assert!(resolve_profile(input("http://[::1]:8090"), None).is_ok());
        assert!(resolve_profile(input("http://[fd00::1]:8090"), None).is_err());
        assert_eq!(
            resolve_profile(input("http://127.0.0.1:8090/"), None)
                .unwrap()
                .remote_url,
            "http://127.0.0.1:8090"
        );
    }

    #[test]
    fn validates_profiles_and_provider_endpoints() {
        let mut phala = input("https://tee.redpill.ai");
        phala.provider = ServiceProvider::Phala;
        assert!(resolve_profile(phala, None).is_err());
        let mut invalid_id = input("https://private.example.com");
        invalid_id.id = "../../key".to_string();
        assert!(resolve_profile(invalid_id, None).is_err());
    }

    #[test]
    fn an_empty_file_is_the_default_settings() {
        let config = parse("").unwrap().value;
        assert_eq!(config, Config::default());
        assert!(config.require_production_os);
        assert!(config.profiles.is_empty());
        assert_eq!(config.runtime_config().remote_url, "https://tee.redpill.ai");
        assert_eq!(config.local_api.port, 4180);
        assert_eq!(config.web_ui.port, WEB_UI_DEFAULT_PORT);
        assert!(config.notifications.enabled);
        assert_eq!(config.appearance, Appearance::System);
        assert_eq!(config.update_channel, None);
    }

    #[test]
    fn partial_tables_keep_their_defaults_and_the_first_profile_is_active() {
        let config = parse(
            r#"
appearance = "dark"

[local-api]
port = 5180

[web-ui]
enabled = true

[profiles.work]
name = "Work"
provider = "custom"
remote-url = "https://private.example.com/"

[profiles.home]
name = "Home"
provider = "redpill"
remote-url = "https://tee.redpill.ai"
auth = { kind = "oauth", account-id = "user_1", scope = { organization-id = "org_1", workspace-id = 7 } }
"#,
        )
        .unwrap()
        .value;
        assert_eq!(config.appearance, Appearance::Dark);
        assert_eq!(config.local_api.port, 5180);
        assert_eq!(config.local_api.listen_address, "127.0.0.1");
        assert!(config.web_ui.enabled && config.web_ui.port == WEB_UI_DEFAULT_PORT);
        assert_eq!(config.active_profile, "work");
        assert_eq!(
            config.profiles["work"].remote_url,
            "https://private.example.com"
        );
        let ProfileAuth::OAuth {
            account_id, scope, ..
        } = &config.profiles["home"].auth
        else {
            panic!("expected an account profile");
        };
        assert_eq!(account_id, "user_1");
        let scope = scope.as_deref().unwrap();
        assert_eq!(scope.organization_id.as_deref(), Some("org_1"));
        assert_eq!(scope.workspace_id, Some(7));
        let views = config.profile_views(|id| (id == "home").then(|| "ref".to_string()));
        assert_eq!(views[0].id, "work");
        assert!(!views[0].credential_saved);
        assert!(views[1].credential_saved);
    }

    #[test]
    fn errors_name_the_line_column_and_key() {
        let error = parse("appearance = \"dark\"\n[local-api]\nport = \"x\"\n").unwrap_err();
        assert!(error.starts_with("config.toml:3:8: "), "{error}");
        let error = parse("appearance = \"dusk\"\n").unwrap_err();
        assert!(error.starts_with("config.toml:1:14: "), "{error}");
        let error = parse("[local-api]\nlisten-address = \"0.0.0.0\"\n").unwrap_err();
        assert_eq!(
            error,
            "config.toml:1:1: local-api: Network listening requires explicit confirmation"
        );
        let error = parse(
            "\n[profiles.work]\nname = \"Work\"\nprovider = \"custom\"\nremote-url = \"http://example.com\"\n",
        )
        .unwrap_err();
        assert!(
            error.starts_with(
                "config.toml:5:14: profiles.work.remote-url: Gateway URL must use HTTPS"
            ),
            "{error}"
        );
        let error = parse("active-profile = \"missing\"\n").unwrap_err();
        assert!(
            error.starts_with("config.toml:1:18: active-profile:"),
            "{error}"
        );
        assert!(parse("[web-ui]\nport = 4180\n")
            .unwrap_err()
            .contains("Local API"));
        assert!(parse("[profiles.\"a b\"]\nname = \"A\"\nprovider = \"custom\"\nremote-url = \"https://a.example\"\n")
            .unwrap_err()
            .contains("Profile ID"));
    }

    #[test]
    fn every_setting_survives_a_round_trip_through_the_file() {
        let mut config = Config {
            active_profile: "work".into(),
            require_production_os: false,
            connect_on_launch: true,
            appearance: Appearance::Dark,
            update_channel: Some(UpdateChannel::Beta),
            auto_cli_registration: Some(false),
            notifications: NotificationPreferences {
                enabled: true,
                gateway: false,
                local_api: false,
                verification: true,
            },
            local_api: ListenConfig {
                port: 5180,
                ..ListenConfig::default()
            },
            web_ui: WebUiConfig {
                enabled: true,
                listen_address: "0.0.0.0".into(),
                allow_network_access: true,
                port: 4190,
                client_host: Some("studio.local".into()),
            },
            profiles: IndexMap::new(),
        };
        config.profiles.insert(
            "work".into(),
            Profile {
                name: "Work".into(),
                provider: ServiceProvider::Redpill,
                remote_url: "https://tee.redpill.ai".into(),
                auth: ProfileAuth::OAuth {
                    account_id: "user_1".into(),
                    account_name: Some("Me".into()),
                    images: Some(AccountImages {
                        user: Some("https://images.example/me.png".into()),
                        organization: None,
                    }),
                    scope: Some(Box::new(AccountScope {
                        organization_id: Some("org_1".into()),
                        organization_slug: Some("org".into()),
                        organization: Some("Org".into()),
                        workspace: None,
                        workspace_slug: None,
                        workspace_id: Some(7),
                    })),
                },
                verified_at: Some(1_700_000_000),
            },
        );
        let text = toml_edit::ser::to_string(&config).unwrap();
        assert_eq!(parse(&text).unwrap().value, config, "{text}");
        assert!(
            !text.contains("listenAddress") && !text.contains("accountId"),
            "{text}"
        );
    }

    #[test]
    fn unknown_keys_are_reported_but_never_reject_the_file() {
        // A typo, a key from a newer version and the 0.2 pre-release camelCase
        // names load with the defaults for those keys, each reported where it is.
        let parsed = parse(
            "apperance = \"dark\"\n\
             [web-ui]\n\
             enabled = true\n\
             listenAddress = \"0.0.0.0\"\n\
             [profiles.work]\n\
             name = \"Work\"\n\
             provider = \"custom\"\n\
             remote-url = \"https://private.example.com\"\n\
             future = { nested = 1 }\n",
        )
        .unwrap();
        assert_eq!(parsed.value.appearance, Appearance::System);
        assert!(parsed.value.web_ui.enabled);
        assert_eq!(parsed.value.web_ui.listen_address, "127.0.0.1");
        assert_eq!(parsed.value.active_profile, "work");
        assert_eq!(
            parsed.unknown,
            [
                "config.toml:1:13: apperance: unknown key, ignored",
                "config.toml:4:17: web-ui.listenAddress: unknown key, ignored",
                "config.toml:9:10: profiles.work.future: unknown key, ignored",
            ]
        );
        // Type errors and invalid values still reject the file.
        assert!(parse("[web-ui]\nenabled = \"yes\"\n").is_err());
    }

    #[test]
    fn ports_cannot_collide_with_local_services() {
        let config = |port| WebUiConfig {
            enabled: true,
            port,
            ..WebUiConfig::default()
        };
        assert_eq!(
            validate_web_ui(&config(WEB_UI_DEFAULT_PORT), 4180)
                .unwrap()
                .endpoint,
            "http://127.0.0.1:4182"
        );
        assert!(validate_web_ui(&config(0), 4180).is_err());
        assert!(validate_web_ui(&config(4180), 4180).is_err());
        assert!(validate_web_ui(&config(4181), 4180).is_err());
        assert!(validate_web_ui(&config(5000), 5000).is_err());
    }

    #[test]
    fn network_listening_needs_confirmation_and_a_reachable_host() {
        let mut config = WebUiConfig {
            enabled: true,
            listen_address: "192.168.1.20".into(),
            ..WebUiConfig::default()
        };
        assert!(validate_web_ui(&config, 4180)
            .unwrap_err()
            .contains("explicit confirmation"));
        config.allow_network_access = true;
        assert_eq!(
            validate_web_ui(&config, 4180).unwrap().endpoint,
            "http://192.168.1.20:4182"
        );
        config.listen_address = "0.0.0.0".into();
        assert!(validate_web_ui(&config, 4180)
            .unwrap_err()
            .contains("Client host"));
        config.client_host = Some("Studio.local".into());
        let listen = validate_web_ui(&config, 4180).unwrap();
        assert_eq!(listen.bind.to_string(), "0.0.0.0:4182");
        assert_eq!(listen.endpoint, "http://studio.local:4182");
    }

    #[test]
    fn schema_describes_every_section() {
        let schema: serde_json::Value = serde_json::from_str(&schema()).unwrap();
        let properties = schema["properties"].as_object().unwrap();
        for key in [
            "active-profile",
            "local-api",
            "web-ui",
            "profiles",
            "notifications",
        ] {
            assert!(properties.contains_key(key), "{key}");
        }
        let definitions = schema["$defs"].as_object().unwrap();
        for (definition, key) in [
            ("Listener", "listen-address"),
            ("WebUi", "allow-network-access"),
            ("Notifications", "local-api"),
            ("Profile", "remote-url"),
        ] {
            assert!(
                definitions[definition]["properties"]
                    .as_object()
                    .unwrap()
                    .contains_key(key),
                "{definition}.{key}"
            );
        }
        // Unknown keys are reported, not rejected, so editors must not flag them as errors.
        assert!(!schema
            .to_string()
            .contains("\"additionalProperties\":false"));
    }
}
