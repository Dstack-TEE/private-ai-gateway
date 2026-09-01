import { contextBridge, ipcRenderer } from "electron";

import { IPC_CHANNELS } from "../shared/channels";
import type {
  DesktopApi,
  GatewayState,
  ReceiptSummary,
  StartGatewayConfig,
} from "../shared/contracts";

const api: DesktopApi = {
  copyText(text: string) {
    return ipcRenderer.invoke(IPC_CHANNELS.copyText, text) as Promise<void>;
  },
  getState() {
    return ipcRenderer.invoke(IPC_CHANNELS.getState) as Promise<GatewayState>;
  },
  listReceipts() {
    return ipcRenderer.invoke(IPC_CHANNELS.listReceipts) as Promise<ReceiptSummary[]>;
  },
  onStateChange(listener: (state: GatewayState) => void) {
    const handler = (_event: Electron.IpcRendererEvent, state: GatewayState) => listener(state);
    ipcRenderer.on(IPC_CHANNELS.stateChanged, handler);
    return () => ipcRenderer.removeListener(IPC_CHANNELS.stateChanged, handler);
  },
  start(config: StartGatewayConfig) {
    return ipcRenderer.invoke(IPC_CHANNELS.start, config) as Promise<GatewayState>;
  },
  stop() {
    return ipcRenderer.invoke(IPC_CHANNELS.stop) as Promise<GatewayState>;
  },
};

contextBridge.exposeInMainWorld("privateAiGateway", api);
