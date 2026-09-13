use std::{fmt, path::PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

#[derive(Parser)]
#[command(
    name = "private-ai-proxy",
    version = crate::protocol::BUILD_VERSION,
    about = "Control the Private AI Proxy",
    long_about = "Control the Private AI Proxy backend, protected connection, profiles, and coding-agent integrations. `private-ai-proxy start` explicitly starts protection and waits for verification. `private-ai-proxy service start` starts the backend; saved connect-on-launch behavior may then start protection automatically."
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
    /// Show backend and gateway state. This does not start the backend.
    Status {
        /// Stream state changes. Human output suppresses unchanged heartbeats; JSON remains NDJSON.
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
    /// Manage the local backend process. Starting it may honor saved connect-on-launch behavior.
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
    /// Inspect or change desktop and Local API settings.
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
    /// Open the installed desktop app. App startup may honor saved connect-on-launch behavior.
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
    /// Emit basic machine-readable Clap command and argument metadata as JSON.
    Schema,
}

#[derive(Subcommand)]
pub(super) enum Service {
    /// Start the local backend. Saved connect-on-launch behavior may start protection afterward.
    Start,
    /// Stop the backend and restore managed agent configurations.
    Stop,
    /// Show backend and gateway state without starting the backend.
    Status,
}

#[derive(Subcommand)]
pub(super) enum App {
    /// Open the installed desktop UI; startup may honor saved connect-on-launch behavior.
    Open,
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

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(super) enum SettingsKey {
    #[value(name = "autoCliRegistration")]
    AutoCliRegistration,
    #[value(name = "notifications")]
    Notifications,
    #[value(name = "connectOnLaunch")]
    ConnectOnLaunch,
    #[value(name = "appearance")]
    Appearance,
    #[value(name = "updateChannel")]
    UpdateChannel,
    #[value(name = "listenAddress")]
    ListenAddress,
    #[value(name = "allowNetworkAccess")]
    AllowNetworkAccess,
    #[value(name = "port")]
    Port,
    #[value(name = "clientHost")]
    ClientHost,
}

impl SettingsKey {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::AutoCliRegistration => "autoCliRegistration",
            Self::Notifications => "notifications",
            Self::ConnectOnLaunch => "connectOnLaunch",
            Self::Appearance => "appearance",
            Self::UpdateChannel => "updateChannel",
            Self::ListenAddress => "listenAddress",
            Self::AllowNetworkAccess => "allowNetworkAccess",
            Self::Port => "port",
            Self::ClientHost => "clientHost",
        }
    }
}

impl fmt::Display for SettingsKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Subcommand)]
pub(super) enum Settings {
    /// Stop protection and reset backend settings, preserving profiles, keys and usage.
    Reset,
    /// Show desktop preferences and Local API settings.
    Show,
    /// Change one setting. This may restart the Local API or protection.
    Set {
        #[arg(value_enum)]
        key: SettingsKey,
        /// Boolean keys use true/false; appearance uses system/light/dark; updateChannel uses beta/stable; notifications is a JSON object with boolean fields.
        value: String,
    },
}
