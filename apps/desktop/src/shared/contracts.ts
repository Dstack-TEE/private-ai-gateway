// Backend DTOs and constants are generated from Rust; this file adds renderer-only shapes.
import type {
  AboutLink,
  Appearance,
  ConfidentialProfileInput,
  AccountBalance,
  AccountBalanceTarget,
  AccountLoginDetails,
  AgentAccessStatus,
  AgentPreview,
  AgentStatus,
  CliRegistration,
  ConnectOptions,
  AppState,
  ImportResult,
  LaunchPreferences,
  ListenAddress,
  ListenConfig,
  LoginPresentation,
  NotificationPreferences,
  ProfileBackup,
  RequestActivity,
  ServiceProvider,
  StartConfig,
  UpdateChannel,
  UpdateInfo,
  UsagePage,
  UsageQuery,
  VerificationCheck,
  WebUiConfig,
} from "./contracts.generated";

export * from "./contracts.generated";

export type CheckStatus = VerificationCheck["status"];

export interface NotificationConfiguration {
  preferences: NotificationPreferences;
  permission: "granted" | "denied" | "notDetermined" | "unknown" | "unsupported";
  alertsEnabled?: boolean;
}

/** What a tray or menu item asks the main window to show or open. */
export type NavigationTarget = "settings" | "agents" | "profiles" | "profile-setup" | "documentation" | "github";

export interface DesktopApi {
  startBackendService(): Promise<AppState>;
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
  saveLocalApiConfig(config: ListenConfig): Promise<AppState>;
  saveWebUi(config: WebUiConfig): Promise<AppState>;
  /** Sets or (with `null`) removes the web UI password. Browsers must prove the current one. */
  setWebUiPassword(password: string | null, currentPassword?: string): Promise<AppState>;
  /** Opens the listening web UI in the system browser; desktop only. */
  openWebUi(): Promise<void>;
  listListenAddresses(): Promise<ListenAddress[]>;
  getNotificationSettings(): Promise<NotificationConfiguration>;
  selectProfileBackup(): Promise<ProfileBackup | null>;
  /** Saves where the user chooses; `false` when they cancel. */
  saveProfileExport(): Promise<boolean>;
  saveDiagnosticsExport(): Promise<boolean>;
  importProfiles(backup: ProfileBackup): Promise<ImportResult>;
  saveNotificationSettings(config: NotificationPreferences): Promise<void>;
  requestNotificationPermission(): Promise<Pick<NotificationConfiguration, "permission" | "alertsEnabled">>;
  openNotificationSettings(): Promise<void>;
  getState(): Promise<AppState>;
  resetSettings(): Promise<AppState>;
  onSettingsReset(listener: () => void): () => void;
  onStateChange(listener: (state: AppState) => void): () => void;
  onNavigate(listener: (target: NavigationTarget) => void): () => void;
  onAgentsChange(listener: () => void): () => void;
  onClientKeyChange(listener: (available: boolean) => void): () => void;
  mainWindowReady(): Promise<void>;
  /** Open a documented, allowlisted project resource in the system browser. */
  openAboutLink(target: AboutLink): Promise<void>;
  openAgentWebsite(agentId: string): Promise<void>;
  openApiKeyPage(provider: ServiceProvider): Promise<void>;
  start(config: StartConfig): Promise<AppState>;
  saveConfiguration(profile: ConfidentialProfileInput, requireProductionOs: boolean, key?: string): Promise<AppState>;
  completeAccountLogin(id: string, callbackUrl: string): Promise<void>;
  beginAccountLogin(profile: ConfidentialProfileInput): Promise<LoginPresentation>;
  pollAccountLogin(id: string): Promise<AccountLoginDetails | null>;
  saveAccountLogin(id: string, profile: ConfidentialProfileInput, requireProductionOs: boolean, workspaceId?: number): Promise<AppState>;
  getAccountDetails(profileId: string): Promise<AccountLoginDetails>;
  getAccountBalance(target: AccountBalanceTarget): Promise<AccountBalance | null>;
  openOrganization(organizationSlug: string): Promise<void>;
  openTopUp(provider: ServiceProvider, scopeSlug?: string): Promise<void>;
  cancelAccountLogin(id: string): Promise<void>;
  activateProfile(profileId: string): Promise<AppState>;
  deleteProfile(profileId: string): Promise<AppState>;
  stop(): Promise<AppState>;
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
