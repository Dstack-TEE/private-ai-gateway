// Backend DTOs and constants are generated from Rust; this file adds renderer-only shapes.
import type {
  AboutLink,
  Appearance,
  ConfidentialProfileInput,
  AccountBalanceTarget,
  CliRegistration,
  AppState,
  LaunchPreference,
  LaunchPreferences,
  ListenConfig,
  NavigationTarget,
  NotificationPermissionStatus,
  NotificationPreferences,
  ProfileBackup,
  ServiceProvider,
  StartConfig,
  UpdateChannel,
  UpdateInfo,
  UsageQuery,
  WebUiConfig,
  UiMethod,
  UiResponses,
} from "./contracts.generated";

export * from "./contracts.generated";

/** What the UI method `M` answers, as the Rust contracts declare it. */
type UiResult<M extends UiMethod> = Promise<UiResponses[M]>;

export interface DesktopApi {
  startBackendService(): UiResult<"start_backend_service">;
  showEditMenu(editable: boolean): Promise<void>;
  getAppearance(): UiResult<"get_appearance">;
  setAppearance(appearance: Appearance): UiResult<"set_appearance">;
  onAppearanceChange(listener: (appearance: Appearance) => void): () => void;
  getAppVersion(): Promise<string>;
  setUpdateChannel(channel: UpdateChannel): Promise<UpdateChannel>;
  prepareUpdate(): Promise<UpdateInfo>;
  restartToUpdate(): Promise<void>;
  getLaunchPreferences(): UiResult<"get_launch_preferences">;
  setLaunchPreference(name: LaunchPreference, enabled: boolean): UiResult<"set_launch_preference">;
  onLaunchPreferencesChange(listener: (preferences: LaunchPreferences) => void): () => void;
  getCliRegistration(): Promise<CliRegistration>;
  setCliRegistration(installed: boolean): Promise<CliRegistration>;
  stopAllAndQuit(): Promise<void>;
  // Desktop-only capabilities are absent in the web UI.
  /** Closes the window as its close button does; the app keeps running. */
  closeWindow: (() => Promise<void>) | undefined;
  /** Quits the app and leaves the background service running. */
  quit: (() => Promise<void>) | undefined;
  copyText(text: string): Promise<void>;
  getClientKey(): UiResult<"get_client_key">;
  rotateClientKey(): UiResult<"rotate_client_key">;
  saveLocalApiConfig(config: ListenConfig): UiResult<"save_local_api_config">;
  saveWebUi(config: WebUiConfig): UiResult<"save_web_ui">;
  /** The web UI password; `null` while only the hash an earlier version kept is set. Not for browsers. */
  getWebUiPassword(): UiResult<"get_web_ui_password">;
  /** Replaces the web UI password with a generated one, signing out every browser. Not for browsers. */
  rotateWebUiPassword(): UiResult<"rotate_web_ui_password">;
  /** Sets a chosen web UI password, signing out every browser. Not for browsers. */
  setWebUiPassword(password: string): UiResult<"set_web_ui_password">;
  /** Opens the listening web UI in the system browser; desktop only. */
  openWebUi(): Promise<void>;
  listListenAddresses(): UiResult<"list_listen_addresses">;
  getNotificationSettings(): UiResult<"get_notification_settings">;
  selectProfileBackup(): Promise<ProfileBackup | null>;
  /** Saves where the user chooses, unless they cancel. */
  saveProfileExport(): Promise<void>;
  saveDiagnosticsExport(): Promise<void>;
  importProfiles(backup: ProfileBackup): UiResult<"import_profiles">;
  saveNotificationSettings(config: NotificationPreferences): UiResult<"save_notification_settings">;
  requestNotificationPermission(): Promise<NotificationPermissionStatus>;
  openNotificationSettings(): Promise<void>;
  getState(): UiResult<"get_state">;
  resetSettings(): UiResult<"reset_settings">;
  onSettingsReset(listener: () => void): () => void;
  onStateChange(listener: (state: AppState) => void): () => void;
  /** A request from the tray or the menu bar; one made before the window listened comes first. Desktop only. */
  onNavigate(listener: (target: NavigationTarget) => void): () => void;
  onAgentsChange(listener: () => void): () => void;
  onClientKeyChange(listener: (available: boolean) => void): () => void;
  /** Open a documented, allowlisted project resource in the system browser. */
  openAboutLink(target: AboutLink): Promise<void>;
  openAgentWebsite(agentId: string): Promise<void>;
  openApiKeyPage(provider: ServiceProvider): Promise<void>;
  start(config: StartConfig): UiResult<"start">;
  /** Stops protection and saves the OS policy the next start uses. */
  setRequireProductionOs(required: boolean): UiResult<"set_require_production_os">;
  saveConfiguration(profile: ConfidentialProfileInput, requireProductionOs: boolean, key?: string): UiResult<"save_configuration">;
  completeAccountLogin(id: string, callbackUrl: string): UiResult<"complete_account_login">;
  beginAccountLogin(profile: ConfidentialProfileInput): UiResult<"begin_account_login">;
  pollAccountLogin(id: string): UiResult<"poll_account_login">;
  saveAccountLogin(id: string, profile: ConfidentialProfileInput, requireProductionOs: boolean, workspaceId?: number): UiResult<"save_account_login">;
  getAccountDetails(profileId: string): UiResult<"get_account_details">;
  getAccountBalance(target: AccountBalanceTarget): UiResult<"get_account_balance">;
  openOrganization(organizationSlug: string): Promise<void>;
  openTopUp(provider: ServiceProvider, scopeSlug?: string): Promise<void>;
  cancelAccountLogin(id: string): UiResult<"cancel_account_login">;
  activateProfile(profileId: string): UiResult<"activate_profile">;
  deleteProfile(profileId: string): UiResult<"delete_profile">;
  stop(): UiResult<"stop">;
  queryUsage(query: UsageQuery): UiResult<"query_usage">;
  getUsageRecord(recordId: string): UiResult<"get_usage_record">;
  /** The signed receipt document a record's audit checked, exactly as the service returned it. */
  getUsageReceipt(recordId: string): UiResult<"get_usage_receipt">;
  listAgents(): UiResult<"list_agents">;
  getAgentAccess(): UiResult<"get_agent_access">;
  requestAgentAccess(): UiResult<"request_agent_access">;
  setAgentConnection(agentId: string, connect: boolean): UiResult<"set_agent_connection">;
  /** Whether Codex's background service still runs with the settings from before a change; `false` when the build cannot stop it. */
  agentServiceRunning(agentId: string): UiResult<"agent_service_running">;
  /** Stops Codex's background service, ending its running sessions. */
  stopAgentService(agentId: string): UiResult<"stop_agent_service">;
}
