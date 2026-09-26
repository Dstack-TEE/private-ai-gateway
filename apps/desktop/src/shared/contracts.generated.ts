// Generated from the Rust contracts by `npm run generate:contracts`. Do not edit.

/**
 * `AppState` as the management API answers and publishes it: the state and
 * the [`Protection`] it presents. Every state-returning command answers one
 * and every state event carries one, each built from its state with `From`,
 * so none can carry a presentation of another state.
 */
export type AppState = { protection: Protection, backendInstance?: string, clientKeyRevision: number, clientKeyAvailable?: boolean,
/**
 * Client connection state; the backend leaves this unset. `false`
 * without an `error` while the client is still starting the backend.
 */
backendConnected?: boolean, wakeMonitorAvailable?: boolean, status: VerificationStatus,
/**
 * True while Settings is verifying a candidate configuration without
 * opening the forwarding session or turning protection on.
 */
configurationVerification: boolean,
/**
 * What the gateway is doing while `verifying`.
 */
progress?: string, remoteUrl?: string,
/**
 * The stable local endpoint agents use; present only while it is bound.
 */
proxyUrl?: string,
/**
 * Why the local endpoint could not be bound; blocks starting and connecting.
 */
endpointError?: string, identity?: ServiceIdentity, checks: Array<VerificationCheck>, activity: Array<RequestActivity>,
/**
 * Stable id and complete persisted totals for the current protection run.
 */
sessionId?: string,
/**
 * Unix seconds when this user protection session began.
 */
protectedSince?: number, reconnecting: boolean,
/**
 * User protection session, independent of the current verified transport.
 */
sessionActive: boolean, sessionUsage: UsageSummary,
/**
 * Changes only when persisted usage changes; renderer queries can depend
 * on this instead of the bounded activity preview.
 */
usageRevision: number, error?: string,
/**
 * The configuration the next start (window or tray toggle) will use.
 */
config: StartConfig, profiles: Array<ConfidentialProfile>, activeProfileId: string, localApi: ListenConfig, apiKeySaved: boolean,
/**
 * The most recently verified catalog. A stopped gateway may retain it for
 * agent projection and readiness state; the proxy still requires a live
 * verified session before forwarding.
 */
catalog?: CatalogSummary, webUi: WebUiStatus, configFiles: ConfigFiles,
/**
 * Changes whenever the agents the backend reports change.
 */
agentsRevision: number, };
/**
 * The verifier's state. `Verifying` lasts until the service identity and
 * the catalog are both in.
 */
export type VerificationStatus = "stopped" | "verifying" | "verified" | "blocked" | "error";
export type Protection = { phase: ProtectionPhase, title: string, tone: Tone, action: ProtectionAction, };
/**
 * What protection is doing, as the user sees it.
 */
export type ProtectionPhase = "starting" | "reconnecting" | "localApiUnavailable" | "verifyingConfiguration" | "verifying" | "blocked" | "interrupted" | "profileRequired" | "notProtected" | "configurationVerified" | "apiKeyRequired" | "protected";
export type ProtectionAction = { operation: ProtectionOperation, label: string, enabled: boolean, };
/**
 * What the protection switch and the tray's protection item do.
 */
export type ProtectionOperation = "start" | "stop" | "setUpProfile";
export type Tone = "success" | "warning" | "danger" | "neutral";
export type VerificationCheck = { id: string, section: string, title: string, status: "pass" | "fail" | "skip" | "info", detail: string, };
export type ServiceIdentity = { teeType: string, trustLevel: string, keysetDigest: string, keysetNotAfter: number, tlsSpki?: string, source: SourceProvenance, serving: string, supportedE2eeVersions: Array<string>, };
export type SourceProvenance = { repoUrl?: string, repoCommit?: string, imageDigest?: string, };
/**
 * One request seen by the local proxy: forwarded through the verifier (with
 * its receipt verdict) or answered locally (rejected before any receipt).
 */
export type RequestActivity = { id: string, sessionId: string, method: string, path: string, model?: string, status: number, streamed: boolean, receiptId?: string, verified: boolean | null, detail: string, at: number,
/**
 * The connected agent that sent it, when it presented a token.
 */
agent?: string,
/**
 * Whether the verifier applied its ACI policy to the body before
 * forwarding; the receipt binds those bytes, not the agent's original.
 */
localPolicyApplied?: boolean,
/**
 * Whether the receipt records a service-side rewrite of the request.
 */
rewritten?: boolean,
/**
 * False means the request was rejected by the local proxy before any
 * bytes left the device.
 */
leftDevice: boolean, inputTokens?: number, outputTokens?: number, cacheReadTokens?: number, cacheWriteTokens?: number, costUsd?: number, };
export type UsageSummary = { requests: number, inputTokens: number, outputTokens: number, cacheReadTokens: number, cacheWriteTokens: number, costUsd: number, protected: number, blockedLocally: number, failedProof: number, };
export type CatalogSummary = { revision: string, fetchedAt: number, models: Array<ModelSummary>,
/**
 * Ids served by an earlier refresh that the service no longer lists.
 */
removed: Array<string>, };
export type ModelSummary = { id: string, name: string, supportedEndpoints?: Array<string>, contextLength?: number, maxOutputLength?: number, isTee?: boolean, inputPricePerMillion?: number, outputPricePerMillion?: number, cacheReadPricePerMillion?: number, cacheWritePricePerMillion?: number, inputModalities: Array<string>, outputModalities: Array<string>, capabilities: Array<string>, description?: string, };
export type ServiceProvider = "phala" | "redpill" | "custom";
/**
 * A provider as the profile editor presents it.
 */
export type ServiceProviderInfo = { id: ServiceProvider, label: string, presetUrl: string | null, keyLabel: string, accountLogin: boolean, workspaces: boolean, callbackUrl: boolean, };
export type ProfileAuth = { "kind": "apiKey" } | { "kind": "oauth", accountId: string, accountName?: string, images?: AccountImages, scope?: AccountScope, };
export type AccountImages = { user: string | null, organization: string | null, };
export type AccountScope = { organizationId?: string | null, organizationSlug?: string | null, organization: string | null, workspace: string | null, workspaceSlug?: string | null, workspaceId: number | null, };
export type ConfidentialProfile = { id: string, credentialRef?: string, name: string, provider: ServiceProvider, remoteUrl: string, auth: ProfileAuth,
/**
 * Non-secret presence metadata, independent of verification history.
 */
credentialSaved: boolean, verifiedAt?: number, };
export type ConfidentialProfileInput = { id: string, name: string, provider: ServiceProvider, remoteUrl: string, };
export type AccountWorkspace = { id: number, name: string, isDefault: boolean, };
export type AccountLoginDetails = { auth: ProfileAuth, workspaces: Array<AccountWorkspace>, };
export type AccountBalance = { balanceUsd: string, canTopUp: boolean, organizationId: string | null, grantedUsd: string | null, scope: AccountScope, };
export type AccountBalanceTarget = { "kind": "login", id: string, } | { "kind": "profile", profileId: string, };
export type LoginPresentation = { id: string, url: string, userCode: string | null, };
export type StartConfig = { remoteUrl: string, requireProductionOs: boolean, };
/**
 * A TCP listener shared by the Local API and the web UI. Non-loopback
 * addresses require `allow_network_access`; see [`crate::listen::resolve`].
 */
export type ListenConfig = { listenAddress: string, allowNetworkAccess: boolean, port: number, clientHost?: string, };
/**
 * The service-hosted browser UI. It is off until the user enables it and
 * listens on loopback unless network access is explicitly allowed. Browsers
 * sign in with a generated password (`pap web-ui password show`).
 */
export type WebUiConfig = { enabled: boolean, listenAddress: string, allowNetworkAccess: boolean, port: number, clientHost?: string, };
/**
 * Listener state of the service-hosted web UI. Never carries the password, its hash or session tokens.
 */
export type WebUiStatus = { enabled: boolean, listenAddress: string, allowNetworkAccess: boolean, port: number, clientHost?: string,
/**
 * Present only while the listener is bound.
 */
url?: string, error?: string, };
/**
 * The settings files the backend reads. Never carries their contents.
 */
export type ConfigFiles = { configPath: string, credentialsPath: string,
/**
 * Why the current file contents are not in effect (the previous settings
 * stay in effect), with the file, line and column.
 */
error?: string,
/**
 * Problems that do not stop the files from applying: unknown keys, which
 * are ignored, and what the 0.1 import could not bring over.
 */
warnings: Array<string>,
/**
 * Changes whenever applied settings change, including external edits.
 */
revision: number, };
export type ListenAddress = { address: string, name: string, };
export type Appearance = "system" | "light" | "dark";
export type UpdateChannel = "beta" | "stable";
export type Installation = "desktopApp" | "desktopPacman" | "deb" | "rpm" | "pacman" | "systemPackage" | "npm" | "portable";
export type UpdateNotice = { installation: Installation, channel: UpdateChannel, currentVersion: string,
/**
 * A newer release in the selected channel.
 */
version: string | null,
/**
 * Shell steps that install `version`, when the installation has them.
 */
commands: Array<string>,
/**
 * Portable archive to extract into a fresh directory.
 */
downloadUrl: string | null, };
/**
 * Desktop notifications. The OS permission is managed by the desktop app.
 */
export type NotificationPreferences = { enabled: boolean, gateway: boolean, localApi: boolean, verification: boolean, };
export type LaunchPreferences = { openAtLogin: boolean, connectOnLaunch: boolean, };
export type ProfileBackup = { version: number, profiles: Array<ProfileConfiguration>, };
export type ProfileConfiguration = { name: string, provider: ServiceProvider, remoteUrl: string, };
export type ImportResult = { imported: number, skipped: number, };
export type UsageQuery = { agent?: string, model?: string, sessionId?: string, since?: number, until?: number, cursor?: string, limit?: number, };
export type UsagePage = { items: Array<RequestActivity>, nextCursor: string | null, summary: UsageSummary, series: Array<UsagePoint>, modelSeries: Array<UsageModelPoint>, agents: Array<string>, models: Array<string>, };
export type UsagePoint = { day: string, requests: number, inputTokens: number, outputTokens: number, tokens: number, costUsd: number, };
export type UsageModelPoint = { day: string, model: string | null, requests: number, tokens: number, costUsd: number, };
export type AgentStatus = { id: string, name: string, configPath: string,
/**
 * The agent's configuration folder exists; each agent creates it on
 * first run, so detection does not depend on how the CLI was installed.
 */
installed: boolean,
/**
 * A connected link, including one suspended until protection resumes.
 */
connected: boolean,
/**
 * A connection record exists (whatever the config now says).
 */
recorded: boolean,
/**
 * The proxy would authorize this agent's token right now: recorded,
 * enabled, config readable and its routing/authentication still managed.
 */
authorized: boolean,
/**
 * Something the user must act on (removed model, incomplete disconnect).
 */
attention?: string, error?: string, repairAction?: AgentRepairAction, };
export type AgentRepairAction = "reconnect" | "disconnect";
export type AgentPreview = { agent: AgentStatus, connect: boolean, changes: Array<ConfigChange>, note: string,
/**
 * Fingerprint of the inputs the preview was computed from; `apply`
 * refuses when it no longer matches.
 */
revision: string, };
/**
 * One config field a connection changes. Sensitive fields never show their
 * values; `None` means absent.
 */
export type ConfigChange = { key: string, before: string | null, after: string | null, sensitive: boolean, };
/**
 * User choices a connection is projected with.
 */
export type ConnectOptions = {
/**
 * Optional default selected from the verified catalog. The full model
 * list is discovered natively or generated from that catalog.
 */
defaultModel?: string, };
export type AgentAccessStatus = "authorized" | "authorizationRequired" | "reauthorizationRequired";
/**
 * The desktop app's update check result for the renderer.
 */
export type UpdateInfo = { enabled: boolean, systemManaged: boolean, currentVersion: string, channel: UpdateChannel, version: string | null,
/**
 * Steps that install `version` when a package manager or the user owns
 * the installation.
 */
upgradeCommands: Array<string>,
/**
 * Portable archive to extract into a fresh directory.
 */
downloadUrl?: string | null, };
/**
 * A `pap cli status|install|uninstall` result; the desktop shell reads it
 * from the CLI's JSON output.
 */
export type CommandRegistration = { executable: string, commandPath: string, installed: boolean, onPath: boolean, };
/**
 * The `pap` command registration the renderer shows, with the error from
 * the desktop app's automatic registration attempt, if any.
 */
export type CliRegistration = { startupError?: string, executable: string, commandPath: string, installed: boolean, onPath: boolean, };
export type DistributionChannel = "direct" | "macAppStore" | "web";
/**
 * What this distribution of the app may offer; the renderer hides the rest.
 */
export type DistributionCapabilities = { channel: DistributionChannel, nativeUpdates: boolean, cliRegistration: boolean, accountPortalLinks: boolean, sandboxHomeAccess: boolean, launchAtLogin: boolean, notifications: boolean, webUi: boolean, };
/**
 * `GET /api/bootstrap` on the web UI; it also tells whether this browser has
 * a session.
 */
export type WebBootstrap = { version: string, };
/**
 * A project resource Settings, the Help menu and the tray link to. The
 * renderer receives the URLs as generated constants.
 */
export type AboutLink = "documentation" | "github" | "aci";
/** A method the shared UI API accepts (`ui_api::Method`). */
export type UiMethod = "get_state" | "start" | "stop" | "set_require_production_os" | "activate_profile" | "delete_profile" | "save_configuration" | "complete_account_login" | "begin_account_login" | "poll_account_login" | "get_account_details" | "get_account_balance" | "cancel_account_login" | "get_client_key" | "rotate_client_key" | "save_local_api_config" | "save_web_ui" | "get_web_ui_password" | "rotate_web_ui_password" | "set_web_ui_password" | "import_profiles" | "export_profiles_content" | "export_diagnostics_content" | "query_usage" | "get_usage_record" | "get_usage_receipt" | "list_agents" | "set_agent_connection" | "start_backend_service" | "save_account_login" | "get_organization_url" | "get_top_up_url" | "list_listen_addresses" | "get_agent_access" | "request_agent_access" | "get_appearance" | "set_appearance" | "get_launch_preferences" | "set_launch_preference" | "get_notification_settings" | "save_notification_settings" | "reset_settings" | "get_update_notice";
export const APPEARANCE_EVENT: string = "pap://appearance";
export const LAUNCH_PREFERENCES_EVENT: string = "pap://launch-preferences";
export const SETTINGS_RESET_EVENT: string = "pap://settings-reset";
export const STATE_EVENT: string = "pap://state";
export const CLIENT_KEY_CHANGED_EVENT: string = "pap://client-key-changed";
export const AGENTS_CHANGED_EVENT: string = "pap://agents-changed";
export const NAVIGATE_EVENT: string = "pap://navigate";
export const CONFIRM_STOP_ALL_EVENT: string = "pap://confirm-stop-all";
export const ABOUT_LINKS: Record<AboutLink, string> = {"documentation":"https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/quickstart.md","github":"https://github.com/Dstack-TEE/private-ai-gateway","aci":"https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/attested-confidential-inference.md"};
export const AGENT_WEBSITES: Readonly<Record<string, string>> = {"claude-code":"https://code.claude.com","codex":"https://developers.openai.com/codex/cli/","hermes":"https://hermes-agent.nousresearch.com","pi":"https://pi.dev","oh-my-pi":"https://omp.sh","opencode":"https://opencode.ai","openclaw":"https://openclaw.ai"};
export const API_KEY_PAGES: Partial<Record<ServiceProvider, string>> = {"phala":"https://cloud.phala.com/dashboard","redpill":"https://www.redpill.ai/dashboard"};
export const SERVICE_PROVIDERS: Readonly<Record<ServiceProvider, ServiceProviderInfo>> = {"phala":{"id":"phala","label":"Phala","presetUrl":"https://inference.phala.com","keyLabel":"Phala API key","accountLogin":true,"workspaces":false,"callbackUrl":false},"redpill":{"id":"redpill","label":"RedPill","presetUrl":"https://tee.redpill.ai","keyLabel":"RedPill API key","accountLogin":true,"workspaces":true,"callbackUrl":true},"custom":{"id":"custom","label":"Custom","presetUrl":null,"keyLabel":"API key","accountLogin":false,"workspaces":false,"callbackUrl":false}};
export const DEFAULT_SERVICE_PROVIDER: ServiceProvider = "redpill";
export const BYLINE: string = "by dstack TEE";
export const INITIAL_STATE: AppState = {"clientKeyRevision":0,"status":"stopped","configurationVerification":false,"checks":[],"activity":[],"reconnecting":false,"sessionActive":false,"sessionUsage":{"requests":0,"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0,"costUsd":0.0,"protected":0,"blockedLocally":0,"failedProof":0},"usageRevision":0,"config":{"remoteUrl":"https://tee.redpill.ai","requireProductionOs":true},"profiles":[],"activeProfileId":"","localApi":{"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4180},"apiKeySaved":false,"webUi":{"enabled":false,"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4182},"configFiles":{"configPath":"","credentialsPath":"","warnings":[],"revision":0},"agentsRevision":0,"protection":{"phase":"profileRequired","title":"Not protected","tone":"neutral","action":{"operation":"setUpProfile","label":"Set Up Profile…","enabled":true}}};
export const UNAVAILABLE_STATE: AppState = {"clientKeyRevision":0,"backendConnected":false,"status":"error","configurationVerification":false,"endpointError":"The background service stopped.","checks":[],"activity":[],"reconnecting":false,"sessionActive":false,"sessionUsage":{"requests":0,"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0,"costUsd":0.0,"protected":0,"blockedLocally":0,"failedProof":0},"usageRevision":0,"error":"The background service is unavailable.","config":{"remoteUrl":"https://tee.redpill.ai","requireProductionOs":true},"profiles":[],"activeProfileId":"","localApi":{"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4180},"apiKeySaved":false,"webUi":{"enabled":false,"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4182},"configFiles":{"configPath":"","credentialsPath":"","warnings":[],"revision":0},"agentsRevision":0,"protection":{"phase":"localApiUnavailable","title":"Local API unavailable","tone":"danger","action":{"operation":"setUpProfile","label":"Set Up Profile…","enabled":false}}};
export const WEB_UI_PASSWORD_MIN_LENGTH: number = 12;
export const WEB_DISTRIBUTION: DistributionCapabilities = {"channel":"web","nativeUpdates":false,"cliRegistration":false,"accountPortalLinks":true,"sandboxHomeAccess":false,"launchAtLogin":false,"notifications":false,"webUi":true};
export const DEFAULT_LOCAL_API_CONFIG: ListenConfig = {"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4180};
export const DEFAULT_WEB_UI_CONFIG: WebUiConfig = {"enabled":false,"listenAddress":"127.0.0.1","allowNetworkAccess":false,"port":4182};
