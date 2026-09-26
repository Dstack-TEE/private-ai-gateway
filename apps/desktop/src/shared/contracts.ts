// Backend DTOs and constants are generated from Rust; this file adds renderer-only shapes.
import type {
  AboutLink,
  Appearance,
  ConfidentialProfileInput,
  Confirmation,
  AccountBalance,
  AccountBalanceTarget,
  AccountLoginDetails,
  AgentAccessStatus,
  AgentStatus,
  CliRegistration,
  AppState,
  ImportResult,
  LaunchPreference,
  LaunchPreferences,
  ListenAddress,
  ListenConfig,
  LoginPresentation,
  NavigationTarget,
  NotificationConfiguration,
  NotificationPermissionStatus,
  NotificationPreferences,
  ProfileBackup,
  RequestActivity,
  ServiceProvider,
  StartConfig,
  UpdateChannel,
  UpdateInfo,
  UsagePage,
  UsageQuery,
  WebUiConfig,
} from "./contracts.generated";

export * from "./contracts.generated";

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
  setLaunchPreference(name: LaunchPreference, enabled: boolean): Promise<LaunchPreferences>;
  onLaunchPreferencesChange(listener: (preferences: LaunchPreferences) => void): () => void;
  getCliRegistration(): Promise<CliRegistration>;
  setCliRegistration(installed: boolean): Promise<CliRegistration>;
  onStopAllRequest(listener: () => void): () => void;
  stopAllAndQuit(): Promise<void>;
  // Desktop-only capabilities are absent in the web UI.
  /** Asks in an alert sheet on the window; the macOS app only. */
  showConfirmation: ((confirmation: Confirmation) => Promise<boolean>) | undefined;
  /** Closes the window as its close button does; the app keeps running. */
  closeWindow: (() => Promise<void>) | undefined;
  /** Quits the app and leaves the background service running. */
  quit: (() => Promise<void>) | undefined;
  copyText(text: string): Promise<void>;
  getClientKey(): Promise<string>;
  rotateClientKey(): Promise<string>;
  saveLocalApiConfig(config: ListenConfig): Promise<AppState>;
  saveWebUi(config: WebUiConfig): Promise<AppState>;
  /** The web UI password; `null` while only the hash an earlier version kept is set. Not for browsers. */
  getWebUiPassword(): Promise<string | null>;
  /** Replaces the web UI password with a generated one, signing out every browser. Not for browsers. */
  rotateWebUiPassword(): Promise<string>;
  /** Sets a chosen web UI password, signing out every browser. Not for browsers. */
  setWebUiPassword(password: string): Promise<AppState>;
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
  requestNotificationPermission(): Promise<NotificationPermissionStatus>;
  openNotificationSettings(): Promise<void>;
  getState(): Promise<AppState>;
  resetSettings(): Promise<AppState>;
  onSettingsReset(listener: () => void): () => void;
  onStateChange(listener: (state: AppState) => void): () => void;
  onNavigate(listener: (target: NavigationTarget) => void): () => void;
  onAgentsChange(listener: () => void): () => void;
  onClientKeyChange(listener: (available: boolean) => void): () => void;
  /** Open a documented, allowlisted project resource in the system browser. */
  openAboutLink(target: AboutLink): Promise<void>;
  openAgentWebsite(agentId: string): Promise<void>;
  openApiKeyPage(provider: ServiceProvider): Promise<void>;
  start(config: StartConfig): Promise<AppState>;
  /** Stops protection and saves the OS policy the next start uses. */
  setRequireProductionOs(required: boolean): Promise<AppState>;
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
  /** The signed receipt document a record's audit checked, exactly as the service returned it. */
  getUsageReceipt(recordId: string): Promise<string | null>;
  listAgents(): Promise<AgentStatus[]>;
  getAgentAccess(): Promise<AgentAccessStatus>;
  requestAgentAccess(): Promise<AgentAccessStatus>;
  setAgentConnection(agentId: string, connect: boolean): Promise<AgentStatus>;
}
