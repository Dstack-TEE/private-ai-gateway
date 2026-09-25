import type {
  AgentAccessStatus,
  AgentPreview,
  AgentStatus,
  Appearance,
  CliRegistration,
  DesktopApi,
  AppState,
  ListenConfig,
  LoginPresentation,
  NotificationConfiguration,
  NotificationPreferences,
  ProfileBackup,
  RequestActivity,
  ServiceProvider,
  StartConfig,
  UiMethod,
  UpdateChannel,
  UpdateInfo,
  UsagePage,
  UsageQuery,
  WebUiConfig,
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
  saveProfileExport(): Promise<void>;
  saveDiagnosticsExport(): Promise<void>;
  requestNotificationPermission(): Promise<Pick<NotificationConfiguration, "permission" | "alertsEnabled">>;
  openNotificationSettings(): Promise<void>;
  mainWindowReady(): Promise<void>;
  openAboutLink: DesktopApi["openAboutLink"];
  openWebUi(): Promise<void>;
  openAgentWebsite(agentId: string): Promise<void>;
  openApiKeyPage(provider: ServiceProvider): Promise<void>;
  presentAccountLogin(login: LoginPresentation): void;
  openOrganization(organizationSlug: string): Promise<void>;
  openTopUp(provider: ServiceProvider, scopeSlug?: string): Promise<void>;
}

export function createDesktopApi(transport: UiTransport, platform: UiPlatform): DesktopApi {
  const call = transport.call.bind(transport);
  const subscribe = transport.subscribe.bind(transport);
  return {
    startBackendService: () => call<AppState>("start_backend_service"),
    showEditMenu: platform.showEditMenu,
    getAppearance: () => call<Appearance>("get_appearance"),
    setAppearance: (appearance) => call("set_appearance", { appearance }),
    onAppearanceChange: (listener) => subscribe("pap://appearance", listener),
    getAppVersion: platform.getAppVersion,
    setUpdateChannel: platform.setUpdateChannel,
    prepareUpdate: platform.prepareUpdate,
    restartToUpdate: platform.restartToUpdate,
    getLaunchPreferences: () => call("get_launch_preferences"),
    setLaunchPreference: (name, enabled) => call("set_launch_preference", { name, enabled }),
    onLaunchPreferencesChange: (listener) => subscribe("pap://launch-preferences", listener),
    getCliRegistration: platform.getCliRegistration,
    setCliRegistration: platform.setCliRegistration,
    onStopAllRequest: (listener) => subscribe("pap://confirm-stop-all", listener),
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
    onSettingsReset: (listener) => subscribe("pap://settings-reset", listener),
    onStateChange: (listener) => subscribe("pap://state", listener),
    onNavigate: (listener) => subscribe("pap://navigate", listener),
    onAgentsChange: (listener) => subscribe("pap://agents-changed", listener),
    onClientKeyChange: (listener) => subscribe("pap://client-key-changed", listener),
    mainWindowReady: platform.mainWindowReady,
    openAboutLink: platform.openAboutLink,
    openAgentWebsite: platform.openAgentWebsite,
    openApiKeyPage: platform.openApiKeyPage,
    start: (config: StartConfig) => call<AppState>("start", { config }),
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
    previewAgent: (agentId, connect, options): Promise<AgentPreview> => call("preview_agent", { agentId, connect, options }),
    applyAgent: (agentId, connect, revision, options): Promise<AgentStatus> => call("apply_agent", { agentId, connect, revision, options }),
  };
}
