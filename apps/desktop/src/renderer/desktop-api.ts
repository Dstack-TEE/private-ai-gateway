import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { confirm } from "@tauri-apps/plugin-dialog";

import type {
  AgentPreview,
  AgentStatus,
  ConfidentialProfileInput,
  ConnectOptions,
  DesktopApi,
  GatewayState,
  LocalApiConfig,
  RequestActivity,
  StartGatewayConfig,
  UsagePage,
  UsageQuery,
  UpdateProgress,
} from "../shared/contracts";

declare global {
  interface Window {
    __GATEWAY_INITIAL_STATE__?: GatewayState;
    __GATEWAY_INITIAL_APPEARANCE__?: "system" | "light" | "dark";
  }
}

// Native windows receive this non-secret snapshot before the renderer starts.
export const initialGatewayState = window.__GATEWAY_INITIAL_STATE__;
delete window.__GATEWAY_INITIAL_STATE__;
export const initialAppearance = window.__GATEWAY_INITIAL_APPEARANCE__;
delete window.__GATEWAY_INITIAL_APPEARANCE__;

export const desktopApi: DesktopApi = {
  startBackendService: () => invoke("start_backend_service"),
  showEditMenu: (editable) => invoke("show_edit_menu", { editable }),
  getAppearance: () => invoke("get_appearance"),
  setAppearance: (appearance) => invoke("set_appearance", { appearance }),
  onAppearanceChange: (listener) => subscribe("gateway://appearance", listener),
  getAppVersion: getVersion,
  getUpdateChannel: () => invoke("get_update_channel"),
  setUpdateChannel: (channel) => invoke("set_update_channel", { channel }),
  checkUpdate: () => invoke("check_update"),
  installUpdate: () => invoke("install_update"),
  onUpdateProgress: (listener) => {
    let disposed = false;
    let received = false;
    let unlisten: (() => void) | undefined;
    // Subscribe before reading the snapshot, without letting a late snapshot
    // overwrite a newer download event.
    void listen<UpdateProgress>("gateway://update-progress", (event) => {
      received = true;
      if (!disposed) listener(event.payload);
    }).then(async (stop) => {
      if (disposed) { stop(); return; }
      unlisten = stop;
      const snapshot = await invoke<UpdateProgress>("get_update_progress");
      if (!disposed && !received) listener(snapshot);
    }).catch(() => {
      if (!disposed) listener({ downloaded: 0, error: "Update progress is unavailable. The update may still be running." });
    });
    return () => { disposed = true; unlisten?.(); };
  },
  getLaunchPreferences: () => invoke("get_launch_preferences"),
  setLaunchPreference: (name, enabled) => invoke("set_launch_preference", { name, enabled }),
  onLaunchPreferencesChange: (listener) => subscribe("gateway://launch-preferences", listener),
  getCliRegistration: () => invoke("get_cli_registration"),
  setCliRegistration: (installed) => invoke("set_cli_registration", { installed }),
  onStopAllRequest: (listener) => subscribe("gateway://confirm-stop-all", listener),
  stopAllAndQuit: () => invoke("stop_all_and_quit"),
  copyText(text: string): Promise<void> {
    return invoke("copy_text", { text });
  },
  getClientKey(): Promise<string> {
    return invoke("get_client_key");
  },
  rotateClientKey(): Promise<string> {
    return invoke("rotate_client_key");
  },
  saveLocalApiConfig(config: LocalApiConfig): Promise<GatewayState> {
    return invoke("save_local_api_config", { config });
  },
  listListenAddresses: () => invoke("list_listen_addresses"),
  getNotificationSettings: () => invoke("get_notification_settings"),
  readProfileBackup: (path) => invoke("read_profile_backup", { path }),
  importProfiles: (backup) => invoke("import_profiles", { backup }),
  exportProfiles: (path) => invoke("export_profiles", { path }),
  exportDiagnostics: (path) => invoke("export_diagnostics", { path }),
  saveNotificationSettings: (config) => invoke("save_notification_settings", { config }),
  requestNotificationPermission: () => invoke("request_notification_permission"),
  openNotificationSettings: () => invoke("open_notification_settings"),
  getState(): Promise<GatewayState> {
    return invoke("get_gateway_state");
  },
  resetSettings: () => invoke("reset_settings"),
  onSettingsReset: (listener) => subscribe("gateway://settings-reset", listener),
  onStateChange(listener: (state: GatewayState) => void): () => void {
    return subscribe("gateway://state", listener);
  },
  onNavigate(listener: (section: "settings" | "agents") => void): () => void {
    return subscribe("gateway://navigate", listener);
  },
  onAgentsChange: (listener) => subscribe("gateway://agents-changed", listener),
  onProfileRepairRequest(listener: () => void): () => void {
    return subscribe("gateway://profile-repair", listener);
  },
  onUsageProofRequest(listener: (recordId: string) => void): () => void {
    return subscribe("gateway://usage-proof", listener);
  },
  onClientKeyChange(listener: (available: boolean) => void): () => void {
    return subscribe("gateway://client-key-changed", listener);
  },
  openNativeDialog(kind, options): Promise<void> {
    return invoke("open_native_dialog", {
      kind,
      repair: options?.repair ?? false,
      recordId: options?.recordId,
      profileId: options?.profileId,
    });
  },
  closeNativeDialog(): Promise<void> {
    return invoke("close_native_dialog");
  },
  nativeDialogReady: () => invoke("native_dialog_ready"),
  mainWindowReady: () => invoke("main_window_ready"),
  onNativeDialogOpen: (listener) => subscribe("gateway://dialog-open", listener),
  onNativeDialogDismissed: (listener) => subscribe("gateway://dialog-dismissed", listener),
  onNativeCloseRequest: (listener) => subscribe("gateway://dialog-close-requested", listener),
  openAboutLink(target: "documentation" | "github"): Promise<void> {
    return invoke("open_about_link", { target });
  },
  openAgentWebsite: (agentId) => invoke("open_agent_website", { agentId }),
  confirm(options): Promise<boolean> {
    return confirm(options.message, {
      title: options.title,
      kind: "warning",
      okLabel: options.confirmLabel,
      cancelLabel: options.cancelLabel ?? "Cancel",
    });
  },

  beginAccountLogin: (profile) => invoke("begin_account_login", { profile }),
  saveAccountLogin: (id, profile, requireProductionOs) => invoke("save_account_login", { id, profile, requireProductionOs }),
  pollAccountLogin: (id) => invoke("poll_account_login", { id }),
  cancelAccountLogin: (id) => invoke("cancel_account_login", { id }),
  start(config: StartGatewayConfig): Promise<GatewayState> {
    return invoke("start_gateway", { config });
  },
  verifyConfiguration(profile: ConfidentialProfileInput, requireProductionOs: boolean, key?: string): Promise<GatewayState> {
    return invoke("verify_configuration", { profile, requireProductionOs, key });
  },
  activateProfile(profileId: string): Promise<GatewayState> {
    return invoke("activate_profile", { profileId });
  },
  deleteProfile(profileId: string): Promise<GatewayState> {
    return invoke("delete_profile", { profileId });
  },
  stop(): Promise<GatewayState> {
    return invoke("stop_gateway");
  },
  clearApiKey(): Promise<GatewayState> {
    return invoke("clear_api_key");
  },
  queryUsage(query: UsageQuery): Promise<UsagePage> {
    return invoke("query_usage", { query });
  },
  getUsageRecord(recordId: string): Promise<RequestActivity> {
    return invoke("get_usage_record", { recordId });
  },
  exportUsageCsv(query: UsageQuery, path: string): Promise<number> {
    return invoke("export_usage_csv", { query, path });
  },
  clearUsage(): Promise<number> {
    return invoke("clear_usage");
  },
  refreshCatalog(): Promise<GatewayState> {
    return invoke("refresh_catalog");
  },
  listAgents(): Promise<AgentStatus[]> {
    return invoke("list_agents");
  },
  disconnectAllAgents(): Promise<AgentStatus[]> {
    return invoke("disconnect_all_agents");
  },
  previewAgent(agentId: string, connect: boolean, options: ConnectOptions): Promise<AgentPreview> {
    return invoke("preview_agent_connection", { agentId, connect, options });
  },
  applyAgent(
    agentId: string,
    connect: boolean,
    revision: string,
    options: ConnectOptions,
  ): Promise<AgentStatus> {
    return invoke("apply_agent_connection", { agentId, connect, revision, options });
  },
};

function subscribe<T>(event: string, listener: (payload: T) => void): () => void {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  try {
    void listen<T>(event, (received) => { if (!disposed) listener(received.payload); }, {
      target: { kind: "WebviewWindow", label: getCurrentWebviewWindow().label },
    }).then(
      (nextUnlisten) => {
        if (disposed) {
          nextUnlisten();
        } else {
          unlisten = nextUnlisten;
        }
      },
      () => undefined,
    );
  } catch {
    return () => undefined;
  }
  return () => {
    disposed = true;
    unlisten?.();
  };
}
