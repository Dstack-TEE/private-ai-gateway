import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
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
} from "../shared/contracts";

declare global {
  interface Window {
    __GATEWAY_INITIAL_STATE__?: GatewayState;
  }
}

// Native windows receive this non-secret snapshot before the renderer starts.
export const initialGatewayState = window.__GATEWAY_INITIAL_STATE__;
delete window.__GATEWAY_INITIAL_STATE__;

export const desktopApi: DesktopApi = {
  checkUpdate: () => invoke("check_update"),
  installUpdate: () => invoke("install_update"),
  onUpdateProgress: (listener) => subscribe("gateway://update-progress", listener),
  getLaunchPreferences: () => invoke("get_launch_preferences"),
  setLaunchPreference: (name, enabled) => invoke("set_launch_preference", { name, enabled }),
  onLaunchPreferencesChange: (listener) => subscribe("gateway://launch-preferences", listener),
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
  getState(): Promise<GatewayState> {
    return invoke("get_gateway_state");
  },
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
  onClientKeyChange(listener: () => void): () => void {
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
    void listen<T>(event, (received) => listener(received.payload)).then(
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
