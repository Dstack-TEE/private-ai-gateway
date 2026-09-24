// Generated from the Rust contracts by `npm run generate:contracts`. Do not edit.

export type GatewayState = { backendInstance?: string, clientKeyRevision: number, clientKeyAvailable?: boolean,
/**
 * Client connection state; the backend leaves this unset.
 */
backendConnected?: boolean, wakeMonitorAvailable?: boolean,
/**
 * `stopped`, `verifying` (identity and catalog not both in), `verified`,
 * `blocked`, or `error`.
 */
status: "stopped" | "verifying" | "verified" | "blocked" | "error",
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
endpointError?: string, identity?: GatewayIdentity, checks: Array<VerificationCheck>, activity: Array<RequestActivity>,
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
config: StartGatewayConfig, profiles: Array<ConfidentialProfile>, activeProfileId: string, localApi: ListenConfig, apiKeySaved: boolean,
/**
 * The most recently verified catalog. A stopped gateway may retain it for
 * agent projection and readiness state; the proxy still requires a live
 * verified session before forwarding.
 */
catalog?: CatalogSummary, webUi: WebUiStatus, };
export type VerificationCheck = { id: string, section: string, title: string, status: "pass" | "fail" | "skip" | "info", detail: string, };
export type GatewayIdentity = { teeType: string, trustLevel: string, keysetDigest: string, keysetNotAfter: number, tlsSpki?: string, source: SourceProvenance, serving: string, supportedE2eeVersions: Array<string>, };
export type SourceProvenance = { repoUrl?: string, repoCommit?: string, imageDigest?: string, };
/**
 * One request seen by the local gateway: forwarded through the verifier (with
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
export type StartGatewayConfig = { remoteUrl: string, requireProductionOs: boolean, };
/**
 * A TCP listener shared by the Local API and the web UI. Non-loopback
 * addresses require `allow_network_access`; see [`crate::listen::resolve`].
 */
export type ListenConfig = { listenAddress: string, allowNetworkAccess: boolean, port: number, clientHost?: string, };
/**
 * The service-hosted browser UI. It is off until the user enables it and
 * listens on loopback unless network access is explicitly allowed.
 */
export type WebUiConfig = { enabled: boolean, listenAddress: string, allowNetworkAccess: boolean, port: number, clientHost?: string, };
/**
 * Listener state of the service-hosted web UI. Never carries login codes or tokens.
 */
export type WebUiStatus = { enabled: boolean, listenAddress: string, allowNetworkAccess: boolean, port: number, clientHost?: string,
/**
 * Present only while the listener is bound.
 */
url?: string, error?: string, };
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
downloadUrl: string | null, channelPublished: boolean, };
export type NotificationPreferences = { enabled: boolean, gateway: boolean, localApi: boolean, verification: boolean, };
export type LaunchPreferences = { openAtLogin: boolean, connectOnLaunch: boolean, };
export type ProfileBackup = { version: number, profiles: Array<ProfileConfiguration>, };
export type ProfileConfiguration = { name: string, provider: ServiceProvider, remoteUrl: string, };
export type ImportResult = { imported: number, skipped: number, };
export type UsageQuery = { agent?: string, model?: string, sessionId?: string, since?: number, until?: number, cursor?: string, limit?: number, };
export type UsagePage = { items: Array<RequestActivity>, nextCursor: string | null, summary: UsageSummary, series: Array<UsagePoint>, modelSeries: Array<UsageModelPoint>, agents: Array<string>, models: Array<string>, };
export type UsagePoint = { day: string, requests: number, inputTokens: number, outputTokens: number, tokens: number, costUsd: number, };
export type UsageModelPoint = { day: string, model: string | null, requests: number, tokens: number, costUsd: number, };
export type AgentStatus = { id: string, name: string, configPath: string, installed: boolean,
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
export type UpdateInfo = { enabled: boolean, systemManaged: boolean, currentVersion: string, channel: UpdateChannel, version: string | null, channelPublished: boolean,
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
 * `GET /api/bootstrap` on the web UI.
 */
export type WebBootstrap = { version: string, distribution: DistributionCapabilities, };
/** A method the shared UI API accepts (`ui_api::Method`). */
export type UiMethod = "startBackendService" | "getState" | "start" | "stop" | "activateProfile" | "deleteProfile" | "saveConfiguration" | "completeAccountLogin" | "beginAccountLogin" | "pollAccountLogin" | "saveAccountLogin" | "getAccountDetails" | "getAccountBalance" | "getOrganizationUrl" | "getTopUpUrl" | "cancelAccountLogin" | "getClientKey" | "rotateClientKey" | "saveLocalApiConfig" | "saveWebUi" | "listListenAddresses" | "importProfiles" | "exportProfilesContent" | "exportDiagnosticsContent" | "queryUsage" | "getUsageRecord" | "listAgents" | "getAgentAccess" | "requestAgentAccess" | "previewAgent" | "applyAgent" | "getAppearance" | "setAppearance" | "getLaunchPreferences" | "setLaunchPreference" | "getNotificationSettings" | "saveNotificationSettings" | "resetSettings" | "getUpdateNotice";
