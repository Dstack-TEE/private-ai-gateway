import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  AgentPreview,
  AgentStatus,
  ConnectOptions,
  DesktopApi,
  GatewayState,
  StartGatewayConfig,
  UsagePage,
  UsageQuery,
} from "../shared/contracts";

export const desktopApi: DesktopApi = {
  copyText(text: string): Promise<void> {
    return invoke("copy_text", { text });
  },
  getClientKey(): Promise<string> {
    return invoke("get_client_key");
  },
  rotateClientKey(): Promise<string> {
    return invoke("rotate_client_key");
  },
  getState(): Promise<GatewayState> {
    return invoke("get_gateway_state");
  },
  onStateChange(listener: (state: GatewayState) => void): () => void {
    return subscribe("gateway://state", listener);
  },
  onNavigate(listener: (section: "settings") => void): () => void {
    return subscribe("gateway://navigate", listener);
  },
  openSupport(): Promise<void> {
    return invoke("open_support");
  },

  start(config: StartGatewayConfig): Promise<GatewayState> {
    return invoke("start_gateway", { config });
  },
  stop(): Promise<GatewayState> {
    return invoke("stop_gateway");
  },
  setApiKey(key: string): Promise<GatewayState> {
    return invoke("set_api_key", { key });
  },
  clearApiKey(): Promise<GatewayState> {
    return invoke("clear_api_key");
  },
  queryUsage(query: UsageQuery): Promise<UsagePage> {
    return invoke("query_usage", { query });
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
