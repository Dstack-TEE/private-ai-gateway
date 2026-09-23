// Backend DTOs are generated from Rust; this file adds renderer-only shapes.
import type {
  Appearance,
  ConfidentialProfileInput,
  AccountBalance,
  AccountBalanceTarget,
  AccountLoginDetails,
  AgentAccessStatus,
  AgentPreview,
  AgentStatus,
  ConnectOptions,
  GatewayState,
  ImportResult,
  LaunchPreferences,
  ListenAddress,
  ListenConfig,
  LoginPresentation,
  NotificationPreferences,
  ProfileBackup,
  RequestActivity,
  ServiceProvider,
  StartGatewayConfig,
  UpdateChannel,
  UsagePage,
  UsageQuery,
  VerificationCheck,
  WebUiConfig,
} from "./contracts.generated";

export type * from "./contracts.generated";

export type GatewayStatus = GatewayState["status"];
export type CheckStatus = VerificationCheck["status"];
export type LocalApiConfig = ListenConfig;

export interface UpdateInfo {
  enabled: boolean;
  systemManaged: boolean;
  currentVersion: string;
  channel: UpdateChannel;
  version?: string | null;
  channelPublished: boolean;
}

export interface NotificationConfiguration {
  preferences: NotificationPreferences;
  permission: "granted" | "denied" | "notDetermined" | "unknown" | "unsupported";
  alertsEnabled?: boolean;
}

export interface ConfirmationOptions {
  title: string;
  message: string;
  confirmLabel: string;
  cancelLabel?: string;
}

export interface CliRegistration {
  executable: string;
  commandPath: string;
  installed: boolean;
  onPath: boolean;
  startupError?: string;
}

export interface DistributionCapabilities {
  channel: "direct" | "macAppStore" | "web";
  nativeUpdates: boolean;
  cliRegistration: boolean;
  accountPortalLinks: boolean;
  sandboxHomeAccess: boolean;
  launchAtLogin: boolean;
  notifications: boolean;
  webUi: boolean;
}

export interface DesktopApi {
  startBackendService(): Promise<GatewayState>;
  showEditMenu(editable: boolean): Promise<void>;
  getAppearance(): Promise<Appearance>;
  setAppearance(appearance: Appearance): Promise<void>;
  onAppearanceChange(listener: (appearance: Appearance) => void): () => void;
  getAppVersion(): Promise<string>;
  setUpdateChannel(channel: UpdateChannel): Promise<UpdateChannel>;
  prepareUpdate(): Promise<UpdateInfo>;
  restartToUpdate(): Promise<void>;
  getLaunchPreferences(): Promise<LaunchPreferences>;
  setLaunchPreference(name: keyof LaunchPreferences, enabled: boolean): Promise<LaunchPreferences>;
  onLaunchPreferencesChange(listener: (preferences: LaunchPreferences) => void): () => void;
  getCliRegistration(): Promise<CliRegistration>;
  setCliRegistration(installed: boolean): Promise<CliRegistration>;
  onStopAllRequest(listener: () => void): () => void;
  stopAllAndQuit(): Promise<void>;
  copyText(text: string): Promise<void>;
  getClientKey(): Promise<string>;
  rotateClientKey(): Promise<string>;
  saveLocalApiConfig(config: LocalApiConfig): Promise<GatewayState>;
  saveWebUi(config: WebUiConfig): Promise<GatewayState>;
  listListenAddresses(): Promise<ListenAddress[]>;
  getNotificationSettings(): Promise<NotificationConfiguration>;
  selectProfileBackup(): Promise<ProfileBackup | null>;
  saveProfileExport(): Promise<void>;
  saveDiagnosticsExport(): Promise<void>;
  readProfileBackup(path: string): Promise<ProfileBackup>;
  importProfiles(backup: ProfileBackup): Promise<ImportResult>;
  exportProfiles(path: string): Promise<void>;
  exportDiagnostics(path: string): Promise<void>;
  saveNotificationSettings(config: NotificationPreferences): Promise<void>;
  requestNotificationPermission(): Promise<Pick<NotificationConfiguration, "permission" | "alertsEnabled">>;
  openNotificationSettings(): Promise<void>;
  getState(): Promise<GatewayState>;
  resetSettings(): Promise<GatewayState>;
  onSettingsReset(listener: () => void): () => void;
  onStateChange(listener: (state: GatewayState) => void): () => void;
  onSurfaceError(listener: (error: SurfaceError) => void): () => void;
  /** A native menu asked the main window to show a section. */
  onNavigate(listener: (section: "settings" | "agents") => void): () => void;
  onAgentsChange(listener: () => void): () => void;
  onProfileRepairRequest(listener: () => void): () => void;
  onUsageProofRequest(listener: (recordId: string) => void): () => void;
  onClientKeyChange(listener: (available: boolean) => void): () => void;
  openNativeDialog(kind: "profiles" | "profile-editor" | "setup-profile" | "privacy" | "local-api" | "usage-proof" | "local-api-example" | "notifications" | "web-ui", options?: { repair?: boolean; recordId?: string; profileId?: string }): Promise<void>;
  nativeDialogReady(): Promise<void>;
  mainWindowReady(): Promise<void>;
  onNativeDialogOpen(listener: (request: { state: GatewayState; repair: boolean; recordId?: string | null; profileId?: string | null; startAfterSave?: boolean }) => void): () => void;
  onNativeDialogDismissed(listener: () => void): () => void;
  onNativeCloseRequest(listener: () => void): () => void;
  closeNativeDialog(): Promise<void>;
  /** Open a documented, allowlisted project resource in the system browser. */
  openAboutLink(target: "documentation" | "github" | "aci"): Promise<void>;
  openAgentWebsite(agentId: string): Promise<void>;
  openApiKeyPage(provider: ServiceProvider): Promise<void>;
  /** Use the platform confirmation dialog for destructive actions. */
  confirm(options: ConfirmationOptions): Promise<boolean>;
  /** Show a platform-native error alert for an explicit user action that failed. */
  showErrorAlert(title: string, message: string): Promise<void>;
  start(config: StartGatewayConfig): Promise<GatewayState>;
  saveConfiguration(profile: ConfidentialProfileInput, requireProductionOs: boolean, key?: string): Promise<GatewayState>;
  completeAccountLogin(id: string, callbackUrl: string): Promise<void>;
  beginAccountLogin(profile: ConfidentialProfileInput): Promise<LoginPresentation>;
  pollAccountLogin(id: string): Promise<AccountLoginDetails | null>;
  saveAccountLogin(id: string, profile: ConfidentialProfileInput, requireProductionOs: boolean, workspaceId?: number): Promise<GatewayState>;
  getAccountDetails(profileId: string): Promise<AccountLoginDetails>;
  getAccountBalance(target: AccountBalanceTarget): Promise<AccountBalance | null>;
  openOrganization(organizationSlug: string): Promise<void>;
  openTopUp(provider: ServiceProvider, scopeSlug?: string): Promise<void>;
  cancelAccountLogin(id: string): Promise<void>;
  activateProfile(profileId: string): Promise<GatewayState>;
  deleteProfile(profileId: string): Promise<GatewayState>;
  stop(): Promise<GatewayState>;
  queryUsage(query: UsageQuery): Promise<UsagePage>;
  getUsageRecord(recordId: string): Promise<RequestActivity>;
  listAgents(): Promise<AgentStatus[]>;
  getAgentAccess(): Promise<AgentAccessStatus>;
  requestAgentAccess(): Promise<AgentAccessStatus>;
  previewAgent(agentId: string, connect: boolean, options: ConnectOptions): Promise<AgentPreview>;
  applyAgent(
    agentId: string,
    connect: boolean,
    revision: string,
    options: ConnectOptions,
  ): Promise<AgentStatus>;
}

export type SurfaceErrorScope = "protection" | "profiles" | "local-api" | "agents" | "usage" | "settings";

export interface SurfaceError {
  scope: SurfaceErrorScope;
  message: string;
}
