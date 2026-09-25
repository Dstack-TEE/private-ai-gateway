import {
  AGENTS_CHANGED_EVENT,
  APPEARANCE_EVENT,
  CLIENT_KEY_CHANGED_EVENT,
  CONFIRM_STOP_ALL_EVENT,
  LAUNCH_PREFERENCES_EVENT,
  NAVIGATE_EVENT,
  SETTINGS_RESET_EVENT,
  STATE_EVENT,
  type AgentAccessStatus,
  type AgentStatus,
  type Appearance,
  type CliRegistration,
  type DesktopApi,
  type DistributionCapabilities,
  type AppState,
  type ListenConfig,
  type LoginPresentation,
  type NotificationConfiguration,
  type NotificationPreferences,
  type ProfileBackup,
  type RequestActivity,
  type ServiceProvider,
  type StartConfig,
  type UiMethod,
  type UpdateChannel,
  type UpdateInfo,
  type UsagePage,
  type UsageQuery,
  type WebUiConfig,
} from "../../shared/contracts";

export interface UiTransport {
  call<T>(method: UiMethod, params?: Record<string, unknown>): Promise<T>;
  subscribe<T>(event: string, listener: (payload: T) => void): () => void;
}

export interface UiPlatform {
  showEditMenu(editable: boolean): Promise<void>;
  getAppVersion(): Promise<string>;
  setUpdateChannel(channel: UpdateChannel): Promise<UpdateChannel>;
  prepareUpdate(): Promise<UpdateInfo>;
  restartToUpdate(): Promise<void>;
  getCliRegistration(): Promise<CliRegistration>;
  setCliRegistration(installed: boolean): Promise<CliRegistration>;
  stopAllAndQuit(): Promise<void>;
  copyText(text: string): Promise<void>;
  selectProfileBackup(): Promise<ProfileBackup | null>;
  saveProfileExport(): Promise<boolean>;
  saveDiagnosticsExport(): Promise<boolean>;
  requestNotificationPermission(): Promise<Pick<NotificationConfiguration, "permission" | "alertsEnabled">>;
  openNotificationSettings(): Promise<void>;
  openAboutLink: DesktopApi["openAboutLink"];
  openWebUi(): Promise<void>;
  openAgentWebsite(agentId: string): Promise<void>;
  openApiKeyPage(provider: ServiceProvider): Promise<void>;
  presentAccountLogin(login: LoginPresentation): void;
  openOrganization(organizationSlug: string): Promise<void>;
  openTopUp(provider: ServiceProvider, scopeSlug?: string): Promise<void>;
}

/** The browser's web UI session: the router signs in before any page loads. */
export interface WebSession {
  /** Whether this browser has a live session; starts the event stream when it does. */
  check(): Promise<boolean>;
  /** Exchanges the web UI password for an `HttpOnly` session cookie. */
  signIn(password: string): Promise<void>;
  signOut(): Promise<void>;
  /** Called with a notice when the session ends, here or on the server. */
  onEnded(listener: (notice: string) => void): () => void;
}

/** What `#backend` provides: the Tauri shell, or the web UI's HTTP API. */
export interface Backend {
  desktopApi: DesktopApi;
  distributionCapabilities: DistributionCapabilities;
  /** Only the web UI has one. */
  session: WebSession | undefined;
}

export function createDesktopApi(transport: UiTransport, platform: UiPlatform): DesktopApi {
  const call = transport.call.bind(transport);
  const subscribe = transport.subscribe.bind(transport);
  return {
    startBackendService: () => call<AppState>("start_backend_service"),
    showEditMenu: platform.showEditMenu,
    getAppearance: () => call<Appearance>("get_appearance"),
    setAppearance: (appearance) => call("set_appearance", { appearance }),
    onAppearanceChange: (listener) => subscribe(APPEARANCE_EVENT, listener),
    getAppVersion: platform.getAppVersion,
    setUpdateChannel: platform.setUpdateChannel,
    prepareUpdate: platform.prepareUpdate,
    restartToUpdate: platform.restartToUpdate,
    getLaunchPreferences: () => call("get_launch_preferences"),
    setLaunchPreference: (name, enabled) => call("set_launch_preference", { name, enabled }),
    onLaunchPreferencesChange: (listener) => subscribe(LAUNCH_PREFERENCES_EVENT, listener),
    getCliRegistration: platform.getCliRegistration,
    setCliRegistration: platform.setCliRegistration,
    onStopAllRequest: (listener) => subscribe(CONFIRM_STOP_ALL_EVENT, listener),
    stopAllAndQuit: platform.stopAllAndQuit,
    copyText: platform.copyText,
    getClientKey: () => call<string>("get_client_key"),
    rotateClientKey: () => call<string>("rotate_client_key"),
    saveLocalApiConfig: (config: ListenConfig) => call("save_local_api_config", { config }),
    saveWebUi: (config: WebUiConfig) => call("save_web_ui", { config }),
    setWebUiPassword: (password, currentPassword) => call("set_web_ui_password", currentPassword === undefined ? { password } : { password, currentPassword }),
    openWebUi: platform.openWebUi,
    listListenAddresses: () => call("list_listen_addresses"),
    getNotificationSettings: () => call("get_notification_settings"),
    selectProfileBackup: platform.selectProfileBackup,
    saveProfileExport: platform.saveProfileExport,
    saveDiagnosticsExport: platform.saveDiagnosticsExport,
    importProfiles: (backup) => call("import_profiles", { backup }),
    saveNotificationSettings: (config: NotificationPreferences) => call("save_notification_settings", { config }),
    requestNotificationPermission: platform.requestNotificationPermission,
    openNotificationSettings: platform.openNotificationSettings,
    getState: () => call<AppState>("get_state"),
    resetSettings: () => call<AppState>("reset_settings"),
    onSettingsReset: (listener) => subscribe(SETTINGS_RESET_EVENT, listener),
    onStateChange: (listener) => subscribe(STATE_EVENT, listener),
    onNavigate: (listener) => subscribe(NAVIGATE_EVENT, listener),
    onAgentsChange: (listener) => subscribe(AGENTS_CHANGED_EVENT, listener),
    onClientKeyChange: (listener) => subscribe(CLIENT_KEY_CHANGED_EVENT, listener),
    openAboutLink: platform.openAboutLink,
    openAgentWebsite: platform.openAgentWebsite,
    openApiKeyPage: platform.openApiKeyPage,
    start: (config: StartConfig) => call<AppState>("start", { config }),
    setRequireProductionOs: (required) => call<AppState>("set_require_production_os", { required }),
    saveConfiguration: (profile, requireProductionOs, key) => call("save_configuration", { profile, requireProductionOs, key }),
    completeAccountLogin: (id, callbackUrl) => call("complete_account_login", { id, callbackUrl }),
    beginAccountLogin: async (profile) => {
      const login = await call<LoginPresentation>("begin_account_login", { profile });
      platform.presentAccountLogin(login);
      return login;
    },
    pollAccountLogin: (id) => call("poll_account_login", { id }),
    saveAccountLogin: (id, profile, requireProductionOs, workspaceId) => call("save_account_login", { id, profile, requireProductionOs, workspaceId }),
    getAccountDetails: (profileId) => call("get_account_details", { profileId }),
    getAccountBalance: (target) => call("get_account_balance", { target }),
    openOrganization: platform.openOrganization,
    openTopUp: platform.openTopUp,
    cancelAccountLogin: (id) => call("cancel_account_login", { id }),
    activateProfile: (profileId) => call<AppState>("activate_profile", { profileId }),
    deleteProfile: (profileId) => call<AppState>("delete_profile", { profileId }),
    stop: () => call<AppState>("stop"),
    queryUsage: (query: UsageQuery): Promise<UsagePage> => call("query_usage", { query }),
    getUsageRecord: (recordId: string): Promise<RequestActivity> => call("get_usage_record", { recordId }),
    listAgents: (): Promise<AgentStatus[]> => call("list_agents"),
    getAgentAccess: (): Promise<AgentAccessStatus> => call("get_agent_access"),
    requestAgentAccess: (): Promise<AgentAccessStatus> => call("request_agent_access"),
    setAgentConnection: (agentId, connect): Promise<AgentStatus> => call("set_agent_connection", { agentId, connect }),
  };
}
