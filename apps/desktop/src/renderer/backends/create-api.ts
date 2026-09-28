import {
  AGENTS_CHANGED_EVENT,
  APPEARANCE_EVENT,
  CLIENT_KEY_CHANGED_EVENT,
  LAUNCH_PREFERENCES_EVENT,
  SETTINGS_RESET_EVENT,
  STATE_EVENT,
  type CliRegistration,
  type DesktopApi,
  type DistributionCapabilities,
  type LoginPresentation,
  type NotificationPermissionStatus,
  type ProfileBackup,
  type ServiceProvider,
  type UiEvent,
  type UiEventPayloads,
  type UiMethod,
  type UiRequests,
  type UiResponses,
  type UpdateChannel,
  type UpdateInfo,
} from "../../shared/contracts";

/** A UI method's parameters, which a method that takes none may omit. */
export type UiParams<M extends UiMethod> = {} extends UiRequests[M] ? [params?: UiRequests[M]] : [params: UiRequests[M]];

export interface UiTransport {
  call<M extends UiMethod>(method: M, ...params: UiParams<M>): Promise<UiResponses[M]>;
  subscribe<E extends UiEvent>(event: E, listener: (payload: UiEventPayloads[E]) => void): () => void;
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
  onNavigate: DesktopApi["onNavigate"];
  closeWindow: DesktopApi["closeWindow"];
  quit: DesktopApi["quit"];
  copyText(text: string): Promise<void>;
  selectProfileBackup(): Promise<ProfileBackup | null>;
  saveProfileExport(): Promise<void>;
  saveDiagnosticsExport(): Promise<void>;
  requestNotificationPermission(): Promise<NotificationPermissionStatus>;
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
  /**
   * Follows the focus of the desktop window for TanStack Query's
   * `focusManager`; a browser keeps the default, page visibility.
   */
  windowFocus: ((setFocused: (focused: boolean) => void) => () => void) | undefined;
}

export function createDesktopApi(transport: UiTransport, platform: UiPlatform): DesktopApi {
  const call = transport.call.bind(transport);
  const subscribe = transport.subscribe.bind(transport);
  return {
    startBackendService: () => call("start_backend_service"),
    showEditMenu: platform.showEditMenu,
    getAppearance: () => call("get_appearance"),
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
    stopAllAndQuit: platform.stopAllAndQuit,
    closeWindow: platform.closeWindow,
    quit: platform.quit,
    copyText: platform.copyText,
    getClientKey: () => call("get_client_key"),
    rotateClientKey: () => call("rotate_client_key"),
    saveLocalApiConfig: (config) => call("save_local_api_config", { config }),
    saveWebUi: (config) => call("save_web_ui", { config }),
    getWebUiPassword: () => call("get_web_ui_password"),
    rotateWebUiPassword: () => call("rotate_web_ui_password"),
    setWebUiPassword: (password) => call("set_web_ui_password", { password }),
    openWebUi: platform.openWebUi,
    listListenAddresses: () => call("list_listen_addresses"),
    getNotificationSettings: () => call("get_notification_settings"),
    selectProfileBackup: platform.selectProfileBackup,
    saveProfileExport: platform.saveProfileExport,
    saveDiagnosticsExport: platform.saveDiagnosticsExport,
    importProfiles: (backup) => call("import_profiles", { backup }),
    saveNotificationSettings: (config) => call("save_notification_settings", { config }),
    requestNotificationPermission: platform.requestNotificationPermission,
    openNotificationSettings: platform.openNotificationSettings,
    getState: () => call("get_state"),
    resetSettings: () => call("reset_settings"),
    onSettingsReset: (listener) => subscribe(SETTINGS_RESET_EVENT, listener),
    onStateChange: (listener) => subscribe(STATE_EVENT, listener),
    onNavigate: platform.onNavigate,
    onAgentsChange: (listener) => subscribe(AGENTS_CHANGED_EVENT, listener),
    onClientKeyChange: (listener) => subscribe(CLIENT_KEY_CHANGED_EVENT, listener),
    openAboutLink: platform.openAboutLink,
    openAgentWebsite: platform.openAgentWebsite,
    openApiKeyPage: platform.openApiKeyPage,
    start: (config) => call("start", { config }),
    setRequireProductionOs: (required) => call("set_require_production_os", { required }),
    saveConfiguration: (profile, requireProductionOs, key) => call("save_configuration", { profile, requireProductionOs, key }),
    completeAccountLogin: (id, callbackUrl) => call("complete_account_login", { id, callbackUrl }),
    beginAccountLogin: async (profile) => {
      const login = await call("begin_account_login", { profile });
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
    activateProfile: (profileId) => call("activate_profile", { profileId }),
    deleteProfile: (profileId) => call("delete_profile", { profileId }),
    stop: () => call("stop"),
    queryUsage: (query) => call("query_usage", { query }),
    getUsageRecord: (recordId) => call("get_usage_record", { recordId }),
    getUsageReceipt: (recordId) => call("get_usage_receipt", { recordId }),
    listAgents: () => call("list_agents"),
    getAgentAccess: () => call("get_agent_access"),
    requestAgentAccess: () => call("request_agent_access"),
    setAgentConnection: (agentId, connect) => call("set_agent_connection", { agentId, connect }),
    agentServiceRunning: (agentId) => call("agent_service_running", { agentId }),
    stopAgentService: (agentId) => call("stop_agent_service", { agentId }),
  };
}
