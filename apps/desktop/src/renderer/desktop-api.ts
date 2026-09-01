import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

import type {
  DesktopApi,
  GatewayState,
  StartGatewayConfig,
} from "../shared/contracts";

export const desktopApi: DesktopApi = {
  copyText(text: string): Promise<void> {
    return invoke("copy_text", { text });
  },
  getState(): Promise<GatewayState> {
    return invoke("get_gateway_state");
  },
  onStateChange(listener: (state: GatewayState) => void): () => void {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    try {
      void listen<GatewayState>("gateway://state", (event) => listener(event.payload)).then(
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
  },
  start(config: StartGatewayConfig): Promise<GatewayState> {
    return invoke("start_gateway", { config });
  },
  stop(): Promise<GatewayState> {
    return invoke("stop_gateway");
  },
};
