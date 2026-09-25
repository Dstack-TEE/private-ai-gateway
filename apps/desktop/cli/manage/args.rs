use std::{fmt, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

#[derive(Parser)]
#[command(
    name = "private-ai-proxy",
    version = desktop_core::protocol::BUILD_VERSION,
    about = "Control the Private AI Proxy",
    long_about = "Control the Private AI Proxy backend, protected connection, profiles, and coding-agent integrations. `private-ai-proxy start` explicitly starts protection and waits for verification. `private-ai-proxy service start` starts the backend; Protect on launch (`connect-on-launch`) may then start protection automatically."
)]
pub(super) struct Cli {
    /// Emit compact JSON instead of human-readable output.
    #[arg(long, global = true)]
    pub(super) json: bool,
    /// Never prompt. Mutations require --yes; credential inputs use stdin flags.
    #[arg(long, visible_alias = "no-interactive", global = true)]
    pub(super) non_interactive: bool,
    /// Approve a command's documented mutation without prompting.
    #[arg(long, global = true)]
    pub(super) yes: bool,
    #[command(subcommand)]
    pub(super) command: Action,
}

#[derive(Subcommand)]
pub(super) enum Action {
    /// Show backend and protection state. This does not start the backend.
    Status {
        /// Stream state changes: the current state, then each change. JSON is one snapshot per line.
        #[arg(long)]
        watch: bool,
    },
    /// Start protection and wait for a verified connection.
    #[command(
        long_about = "Start the local backend if needed, optionally select a profile, then start protection and wait for verification. This may change the active profile and starts network activity toward the configured service."
    )]
    Start {
        /// Select this saved profile before starting protection.
        #[arg(long)]
        profile: Option<String>,
        /// Maximum verification wait in seconds. The backend may continue verifying after timeout.
        #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u64).range(1..=300))]
        timeout: u64,
    },
    /// Stop protection and restore managed agent configurations; keep the backend running.
    Stop,
    /// Manage the local backend process. Starting it may start protection when Protect on launch (`connect-on-launch`) is on.
    Service {
        #[command(subcommand)]
        command: Service,
    },
    /// Sign in, list, inspect, verify, import, export, select, or delete service profiles.
    Profiles {
        #[command(subcommand)]
        command: Profiles,
    },
    /// Inspect or change supported coding-agent configurations.
    Agents {
        #[command(subcommand)]
        command: Agents,
    },
    /// Inspect models from the last verified catalog.
    Models {
        #[command(subcommand)]
        command: Models,
    },
    /// Query or export local usage records.
    Usage {
        #[command(subcommand)]
        command: Usage,
    },
    /// Inspect or change settings (config.toml; API keys and passwords are in credentials.toml).
    Settings {
        #[command(subcommand)]
        command: Settings,
    },
    /// Manage the Local API token and active profile credential.
    Token {
        #[command(subcommand)]
        command: Token,
    },
    /// Manage installation of the private-ai-proxy command.
    Cli {
        #[command(subcommand)]
        command: Registration,
    },
    /// Open the installed desktop app. Its backend may start protection when Protect on launch (`connect-on-launch`) is on.
    App {
        #[command(subcommand)]
        command: App,
    },
    /// Report independent local installation and backend diagnostics without starting services.
    Doctor,
    /// Export a redacted diagnostics report to a new file.
    Diagnostics {
        /// New destination path. Existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
    },
    /// Generate shell completions from the current command definition.
    Completions {
        /// Shell whose completion script should be written to stdout.
        #[arg(value_enum)]
        shell: Shell,
    },
}

#[derive(Subcommand)]
pub(super) enum Service {
    /// Start the local backend. Protect on launch (`connect-on-launch`) may start protection afterward.
    Start,
    /// Stop the backend and restore managed agent configurations.
    Stop,
    /// Show backend and protection state without starting the backend.
    Status,
}

#[derive(Subcommand)]
pub(super) enum App {
    /// Open the installed desktop UI; its backend may start protection when Protect on launch (`connect-on-launch`) is on.
    /// Without a desktop app or graphical session, or with --web, open or print the
    /// service-hosted web UI address (offering to enable it once a password is set).
    Open {
        /// Open or print the web UI address instead of opening the desktop app.
        #[arg(long)]
        web: bool,
    },
}

#[derive(Subcommand)]
pub(super) enum Registration {
    /// Show whether private-ai-proxy is registered on PATH.
    Status,
    /// Register private-ai-proxy in a user-writable directory.
    Install {
        /// Installation directory; omit to use the platform default.
        #[arg(long)]
        directory: Option<PathBuf>,
    },
    /// Remove a private-ai-proxy registration previously installed by this app.
    Uninstall {
        /// Registration directory; omit to use the platform default.
        #[arg(long)]
        directory: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub(super) enum Token {
    /// Rotate the Local API token, immediately revoking the previous token.
    Rotate,
    /// Print the Local API token to stdout. Treat output as a secret.
    Show,
    /// Remove the active profile's stored service credential.
    ClearCredential,
}

#[derive(Subcommand)]
pub(super) enum Models {
    /// List models from the verified catalog.
    List {
        /// Re-fetch the catalog from the verified service before listing it.
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub(super) enum Provider {
    Phala,
    Redpill,
    Custom,
}

#[derive(Args)]
pub(super) struct AccountLoginOptions {
    /// Existing or new profile ID.
    pub id: String,
    /// Provider for a new profile (defaults to RedPill).
    #[arg(long, value_enum)]
    pub provider: Option<Provider>,
    #[arg(long)]
    pub name: Option<String>,
    /// Workspace ID; required if the organization has several workspaces.
    #[arg(long)]
    pub workspace: Option<i64>,
    /// Read a pasted loopback callback URL from stdin (RedPill only).
    #[arg(long)]
    pub callback_stdin: bool,
    /// Print the authorization URL without launching a browser.
    #[arg(long)]
    pub no_browser: bool,
    #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..=900))]
    pub timeout: u64,
}

#[derive(Subcommand)]
pub(super) enum Profiles {
    /// List saved profile metadata. Credentials are never returned.
    List,
    /// Show one saved profile. Credentials are never returned.
    Show {
        /// Saved profile ID.
        id: String,
    },
    /// Import unverified profile metadata without credentials.
    Import {
        /// Versioned profile backup JSON file.
        file: PathBuf,
    },
    /// Export profile metadata without credentials or verification evidence.
    Export {
        /// New destination path. Existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
    },
    /// Sign in to Phala or RedPill and save an account profile. Protection starts separately.
    Login(AccountLoginOptions),
    /// Save a new profile and credential; protection verifies it when started.
    Add {
        #[arg(long, help = "Unique profile ID")]
        id: String,
        #[arg(long, help = "Display name")]
        name: String,
        #[arg(long, help = "ACI service endpoint URL")]
        url: String,
        #[arg(long, value_enum, default_value_t = Provider::Custom)]
        provider: Provider,
        /// Read the credential from stdin instead of a hidden terminal prompt.
        #[arg(long)]
        key_stdin: bool,
        /// Permit a verified development OS image for this profile.
        #[arg(long)]
        allow_development_os: bool,
    },
    /// Validate and save a profile, optionally replacing its credential.
    Verify {
        /// Saved profile ID.
        id: String,
        /// Read a replacement credential from stdin.
        #[arg(long)]
        key_stdin: bool,
    },
    /// Edit and save an existing profile.
    #[command(
        long_about = "Edit and save an existing profile. Protection verifies the configuration when started. Changing provider or endpoint requires a new credential via --key-stdin or a hidden terminal prompt; the old credential is never sent to a new target."
    )]
    Edit {
        /// Saved profile ID.
        id: String,
        /// New display name.
        #[arg(long)]
        name: Option<String>,
        /// New ACI service endpoint URL.
        #[arg(long)]
        url: Option<String>,
        /// New service provider type.
        #[arg(long, value_enum)]
        provider: Option<Provider>,
        /// Read a replacement credential from stdin.
        #[arg(long)]
        key_stdin: bool,
        /// Permit a verified development OS image; otherwise retain the current global policy.
        #[arg(long, conflicts_with = "require_production_os")]
        allow_development_os: bool,
        /// Require a production OS image; otherwise retain the current global policy.
        #[arg(long, conflicts_with = "allow_development_os")]
        require_production_os: bool,
    },
    /// Make a saved profile active.
    Use { id: String },
    /// Delete a profile and its stored credential.
    Remove { id: String },
}

#[derive(Subcommand)]
pub(super) enum Agents {
    /// List supported agents and their actual configuration state.
    List,
    /// Connect an agent to the Local API.
    Connect {
        /// Agent ID reported by `private-ai-proxy agents list`.
        id: String,
        /// Optional default model from the verified catalog.
        #[arg(long)]
        model: Option<String>,
        /// Preview changes and return a revision without applying them.
        #[arg(long, conflicts_with = "revision")]
        dry_run: bool,
        /// Apply exactly this previously previewed revision without previewing again.
        #[arg(long)]
        revision: Option<String>,
    },
    /// Disconnect an agent and restore its managed configuration.
    Disconnect {
        /// Agent ID reported by `private-ai-proxy agents list`.
        id: String,
        /// Preview changes and return a revision without applying them.
        #[arg(long, conflicts_with = "revision")]
        dry_run: bool,
        /// Apply exactly this previously previewed revision without previewing again.
        #[arg(long)]
        revision: Option<String>,
    },
    /// Disconnect all managed agents and restore their configurations.
    DisconnectAll,
}

#[derive(Args)]
pub(super) struct UsageFilter {
    /// Match an agent ID.
    #[arg(long)]
    pub(super) agent: Option<String>,
    /// Match a model ID.
    #[arg(long)]
    pub(super) model: Option<String>,
    /// Match a session ID.
    #[arg(long)]
    pub(super) session: Option<String>,
    /// Include records at or after this Unix timestamp in seconds.
    #[arg(long)]
    pub(super) since: Option<u64>,
    /// Include records at or before this Unix timestamp in seconds.
    #[arg(long)]
    pub(super) until: Option<u64>,
}

#[derive(Args)]
pub(super) struct Pagination {
    /// Continue after a nextCursor returned by an earlier list response.
    #[arg(long)]
    pub(super) cursor: Option<String>,
    /// Maximum records to return.
    #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u64).range(1..=100))]
    pub(super) limit: u64,
}

#[derive(Subcommand)]
pub(super) enum Usage {
    /// List a page of usage records.
    List {
        #[command(flatten)]
        filter: UsageFilter,
        #[command(flatten)]
        page: Pagination,
    },
    /// Show one usage record by ID.
    Show { id: String },
    /// Export every matching record to a new CSV file; pagination does not apply.
    Export {
        #[command(flatten)]
        filter: UsageFilter,
        /// New destination path. Existing files are never overwritten.
        #[arg(long)]
        output: PathBuf,
        #[arg(long, default_value = "csv", value_parser = ["csv"])]
        format: String,
    },
    /// Permanently delete all local usage history.
    Clear,
}

/// A `settings set` key: the dotted path of the setting in `config.toml`
/// (`git config` and `cargo config get` name keys the same way).
/// `web-ui.password` sets the hash kept in `credentials.toml`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub(super) enum SettingsKey {
    #[value(name = "auto-cli-registration", alias = "autoCliRegistration")]
    AutoCliRegistration,
    #[value(name = "notifications.enabled")]
    NotificationsEnabled,
    #[value(name = "notifications.gateway")]
    NotificationsGateway,
    #[value(name = "notifications.local-api")]
    NotificationsLocalApi,
    #[value(name = "notifications.verification")]
    NotificationsVerification,
    /// Deprecated: all notification settings as one JSON object.
    #[value(name = "notifications", hide = true)]
    Notifications,
    #[value(name = "connect-on-launch", alias = "connectOnLaunch")]
    ConnectOnLaunch,
    #[value(name = "appearance")]
    Appearance,
    #[value(name = "update-channel", alias = "updateChannel")]
    UpdateChannel,
    #[value(name = "local-api.listen-address", alias = "listenAddress")]
    ListenAddress,
    #[value(name = "local-api.allow-network-access", alias = "allowNetworkAccess")]
    AllowNetworkAccess,
    #[value(name = "local-api.port", alias = "port")]
    Port,
    #[value(name = "local-api.client-host", alias = "clientHost")]
    ClientHost,
    #[value(name = "web-ui.enabled", alias = "webUi")]
    WebUi,
    #[value(name = "web-ui.port", alias = "webUiPort")]
    WebUiPort,
    #[value(name = "web-ui.listen-address", alias = "webUiListenAddress")]
    WebUiListenAddress,
    #[value(
        name = "web-ui.allow-network-access",
        alias = "webUiAllowNetworkAccess"
    )]
    WebUiAllowNetworkAccess,
    #[value(name = "web-ui.client-host", alias = "webUiClientHost")]
    WebUiClientHost,
    #[value(name = "web-ui.password", alias = "webUiPassword")]
    WebUiPassword,
}

impl SettingsKey {
    pub(super) fn as_str(self) -> String {
        self.to_possible_value()
            .map(|value| value.get_name().to_string())
            .unwrap_or_default()
    }
}

impl fmt::Display for SettingsKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.as_str())
    }
}

/// A key as typed. The flat camelCase names of 0.1 still work as hidden
/// aliases and print a deprecation warning, as `git config` keeps accepting
/// renamed keys; they are removed in 0.3.
#[derive(Clone, Debug)]
pub(super) struct SettingsKeyArg {
    pub(super) key: SettingsKey,
    typed: String,
}

impl SettingsKeyArg {
    /// The warning for a deprecated spelling, if this is one.
    pub(super) fn deprecation(&self) -> Option<String> {
        let replacement = match self.key {
            SettingsKey::Notifications => "`notifications.enabled`, `notifications.gateway`, `notifications.local-api` or `notifications.verification`".to_string(),
            key if key.as_str() != self.typed => format!("`{key}`"),
            _ => return None,
        };
        Some(format!(
            "warning: `{}` is deprecated and will be removed in 0.3; use {replacement}",
            self.typed
        ))
    }
}

#[derive(Clone)]
pub(super) struct SettingsKeyParser;

impl clap::builder::TypedValueParser for SettingsKeyParser {
    type Value = SettingsKeyArg;

    fn parse_ref(
        &self,
        command: &clap::Command,
        argument: Option<&clap::Arg>,
        value: &std::ffi::OsStr,
    ) -> Result<SettingsKeyArg, clap::Error> {
        let key = clap::builder::EnumValueParser::<SettingsKey>::new()
            .parse_ref(command, argument, value)?;
        Ok(SettingsKeyArg {
            key,
            typed: value.to_string_lossy().into_owned(),
        })
    }

    fn possible_values(
        &self,
    ) -> Option<Box<dyn Iterator<Item = clap::builder::PossibleValue> + '_>> {
        Some(Box::new(
            SettingsKey::value_variants()
                .iter()
                .filter_map(ValueEnum::to_possible_value),
        ))
    }
}

#[derive(Subcommand)]
pub(super) enum Settings {
    /// Stop protection and reset backend settings, preserving profiles, keys and usage.
    Reset,
    /// Show the settings in effect, the settings file paths and web UI status. Never shows secrets.
    Show,
    /// Print the JSON Schema of config.toml for editors and validators.
    Schema,
    /// Change one setting. This may restart the Local API or protection.
    Set {
        /// The setting's dotted path in config.toml, for example web-ui.enabled.
        #[arg(value_parser = SettingsKeyParser)]
        key: SettingsKeyArg,
        /// Boolean keys use true/false; appearance uses system/light/dark; update-channel uses beta/stable. web-ui.password takes no value (use --value-stdin or the hidden prompt); "" removes it.
        value: Option<String>,
        /// Read the web-ui.password value from stdin instead of a hidden terminal prompt.
        #[arg(long)]
        value_stdin: bool,
    },
}
