import type {
  AgentAccessStatus,
  AgentPreview,
  AgentStatus,
  Appearance,
  CliRegistration,
  ConfirmationOptions,
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
  readProfileBackup(path: string): Promise<ProfileBackup>;
  exportProfiles(path: string): Promise<void>;
  exportDiagnostics(path: string): Promise<void>;
  requestNotificationPermission(): Promise<Pick<NotificationConfiguration, "permission" | "alertsEnabled">>;
  openNotificationSettings(): Promise<void>;
  openNativeDialog: DesktopApi["openNativeDialog"];
  closeNativeDialog(): Promise<void>;
  nativeDialogReady(): Promise<void>;
  mainWindowReady(): Promise<void>;
  openAboutLink: DesktopApi["openAboutLink"];
  openAgentWebsite(agentId: string): Promise<void>;
  openApiKeyPage(provider: ServiceProvider): Promise<void>;
  confirm(options: ConfirmationOptions): Promise<boolean>;
  showErrorAlert(title: string, message: string): Promise<void>;
  presentAccountLogin(login: LoginPresentation): void;
  openOrganization(organizationSlug: string): Promise<void>;
  openTopUp(provider: ServiceProvider, scopeSlug?: string): Promise<void>;
}

export function createDesktopApi(transport: UiTransport, platform: UiPlatform): DesktopApi {
  const call = transport.call.bind(transport);
  const subscribe = transport.subscribe.bind(transport);
  return {
    startBackendService: () => call<AppState>("startBackendService"),
    showEditMenu: platform.showEditMenu,
    getAppearance: () => call<Appearance>("getAppearance"),
    setAppearance: (appearance) => call("setAppearance", { appearance }),
    onAppearanceChange: (listener) => subscribe("pap://appearance", listener),
    getAppVersion: platform.getAppVersion,
    setUpdateChannel: platform.setUpdateChannel,
    prepareUpdate: platform.prepareUpdate,
    restartToUpdate: platform.restartToUpdate,
    getLaunchPreferences: () => call("getLaunchPreferences"),
    setLaunchPreference: (name, enabled) => call("setLaunchPreference", { name, enabled }),
    onLaunchPreferencesChange: (listener) => subscribe("pap://launch-preferences", listener),
    getCliRegistration: platform.getCliRegistration,
    setCliRegistration: platform.setCliRegistration,
    onStopAllRequest: (listener) => subscribe("pap://confirm-stop-all", listener),
    stopAllAndQuit: platform.stopAllAndQuit,
    copyText: platform.copyText,
    getClientKey: () => call<string>("getClientKey"),
    rotateClientKey: () => call<string>("rotateClientKey"),
    saveLocalApiConfig: (config: ListenConfig) => call("saveLocalApiConfig", { config }),
    saveWebUi: (config: WebUiConfig) => call("saveWebUi", { config }),
    listListenAddresses: () => call("listListenAddresses"),
    getNotificationSettings: () => call("getNotificationSettings"),
    selectProfileBackup: platform.selectProfileBackup,
    saveProfileExport: platform.saveProfileExport,
    saveDiagnosticsExport: platform.saveDiagnosticsExport,
    readProfileBackup: platform.readProfileBackup,
    importProfiles: (backup) => call("importProfiles", { backup }),
    exportProfiles: platform.exportProfiles,
    exportDiagnostics: platform.exportDiagnostics,
    saveNotificationSettings: (config: NotificationPreferences) => call("saveNotificationSettings", { config }),
    requestNotificationPermission: platform.requestNotificationPermission,
    openNotificationSettings: platform.openNotificationSettings,
    getState: () => call<AppState>("getState"),
    resetSettings: () => call<AppState>("resetSettings"),
    onSettingsReset: (listener) => subscribe("pap://settings-reset", listener),
    onStateChange: (listener) => subscribe("pap://state", listener),
    onSurfaceError: (listener) => subscribe("pap://surface-error", listener),
    onNavigate: (listener) => subscribe("pap://navigate", listener),
    onAgentsChange: (listener) => subscribe("pap://agents-changed", listener),
    onProfileRepairRequest: (listener) => subscribe("pap://profile-repair", listener),
    onUsageProofRequest: (listener) => subscribe("pap://usage-proof", listener),
    onClientKeyChange: (listener) => subscribe("pap://client-key-changed", listener),
    openNativeDialog: platform.openNativeDialog,
    closeNativeDialog: platform.closeNativeDialog,
    nativeDialogReady: platform.nativeDialogReady,
    mainWindowReady: platform.mainWindowReady,
    onNativeDialogOpen: (listener) => subscribe("pap://dialog-open", listener),
    onNativeDialogDismissed: (listener) => subscribe("pap://dialog-dismissed", listener),
    onNativeCloseRequest: (listener) => subscribe("pap://dialog-close-requested", listener),
    openAboutLink: platform.openAboutLink,
    openAgentWebsite: platform.openAgentWebsite,
    openApiKeyPage: platform.openApiKeyPage,
    confirm: platform.confirm,
    showErrorAlert: platform.showErrorAlert,
    start: (config: StartConfig) => call<AppState>("start", { config }),
    saveConfiguration: (profile, requireProductionOs, key) => call("saveConfiguration", { profile, requireProductionOs, key }),
    completeAccountLogin: (id, callbackUrl) => call("completeAccountLogin", { id, callbackUrl }),
    beginAccountLogin: async (profile) => {
      const login = await call<LoginPresentation>("beginAccountLogin", { profile });
      platform.presentAccountLogin(login);
      return login;
    },
    pollAccountLogin: (id) => call("pollAccountLogin", { id }),
    saveAccountLogin: (id, profile, requireProductionOs, workspaceId) => call("saveAccountLogin", { id, profile, requireProductionOs, workspaceId }),
    getAccountDetails: (profileId) => call("getAccountDetails", { profileId }),
    getAccountBalance: (target) => call("getAccountBalance", { target }),
    openOrganization: platform.openOrganization,
    openTopUp: platform.openTopUp,
    cancelAccountLogin: (id) => call("cancelAccountLogin", { id }),
    activateProfile: (profileId) => call<AppState>("activateProfile", { profileId }),
    deleteProfile: (profileId) => call<AppState>("deleteProfile", { profileId }),
    stop: () => call<AppState>("stop"),
    queryUsage: (query: UsageQuery): Promise<UsagePage> => call("queryUsage", { query }),
    getUsageRecord: (recordId: string): Promise<RequestActivity> => call("getUsageRecord", { recordId }),
    listAgents: (): Promise<AgentStatus[]> => call("listAgents"),
    getAgentAccess: (): Promise<AgentAccessStatus> => call("getAgentAccess"),
    requestAgentAccess: (): Promise<AgentAccessStatus> => call("requestAgentAccess"),
    previewAgent: (agentId, connect, options): Promise<AgentPreview> => call("previewAgent", { agentId, connect, options }),
    applyAgent: (agentId, connect, revision, options): Promise<AgentStatus> => call("applyAgent", { agentId, connect, revision, options }),
  };
}
