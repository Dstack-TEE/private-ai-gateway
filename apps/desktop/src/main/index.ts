import path from "node:path";
import { pathToFileURL } from "node:url";

import {
  app,
  BrowserWindow,
  clipboard,
  ipcMain,
  Menu,
  nativeImage,
  Tray,
  type IpcMainInvokeEvent,
} from "electron";

import { AciSidecar } from "./aci-sidecar";
import { buildTrayMenu, statusLabel } from "./tray-menu";
import { IPC_CHANNELS } from "../shared/channels";
import type { GatewayState, StartGatewayConfig } from "../shared/contracts";

let mainWindow: BrowserWindow | undefined;
let tray: Tray | undefined;
let unsubscribeState: (() => void) | undefined;
let quitting = false;
let lastStartConfig: StartGatewayConfig = {
  remoteUrl: "https://tee.redpill.ai",
  requireProductionOs: false,
};

function resolveAciExecutable(): string {
  const executableName = process.platform === "win32" ? "aci.exe" : "aci";
  if (!app.isPackaged) {
    const override = process.env.ACI_DESKTOP_CLI?.trim();
    if (override) {
      return path.resolve(override);
    }
    return path.resolve(app.getAppPath(), "../..", "target/debug", executableName);
  }
  return path.join(
    process.resourcesPath,
    "native",
    `${process.platform}-${process.arch}`,
    executableName,
  );
}

const sidecar = new AciSidecar({ executablePath: resolveAciExecutable() });

function createWindow(): BrowserWindow {
  const window = new BrowserWindow({
    width: 1120,
    height: 760,
    minWidth: 780,
    minHeight: 600,
    backgroundColor: "#f4f6f5",
    show: false,
    title: "Private AI Gateway",
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      preload: path.join(__dirname, "../preload/index.cjs"),
    },
  });
  window.loadFile(path.join(__dirname, "../renderer/index.html"));
  window.once("ready-to-show", () => window.show());
  window.on("close", (event) => {
    if (!quitting) {
      event.preventDefault();
      window.hide();
    }
  });
  return window;
}

function showMainWindow(): void {
  if (!mainWindow || mainWindow.isDestroyed()) {
    mainWindow = createWindow();
    return;
  }
  if (mainWindow.isMinimized()) {
    mainWindow.restore();
  }
  mainWindow.show();
  mainWindow.focus();
}

function createTrayIcon(): Electron.NativeImage {
  const iconRoot = app.isPackaged
    ? path.join(process.resourcesPath, "tray")
    : path.join(app.getAppPath(), "assets/tray");
  const image = nativeImage.createFromPath(path.join(iconRoot, "trayTemplate.png"));
  if (image.isEmpty()) {
    throw new Error("System menu bar icon is missing or invalid");
  }
  if (process.platform === "darwin") {
    image.setTemplateImage(true);
  }
  return image.resize({ width: 18, height: 18 });
}

function startFromTray(): void {
  void sidecar.start(lastStartConfig).catch(() => showMainWindow());
}

function stopFromTray(): void {
  void sidecar.stop().catch(() => showMainWindow());
}

function updateTray(state: GatewayState): void {
  if (!tray) {
    return;
  }
  tray.setToolTip(`Private AI Gateway - ${statusLabel(state.status)}`);
  const template = buildTrayMenu(state, {
    copyEndpoint: (endpoint) => clipboard.writeText(endpoint),
    openWindow: showMainWindow,
    quit: () => app.quit(),
    start: startFromTray,
    stop: stopFromTray,
  });
  tray.setContextMenu(Menu.buildFromTemplate(template));
}

function createTray(): Tray {
  const nextTray = new Tray(createTrayIcon());
  nextTray.on("click", showMainWindow);
  return nextTray;
}

function assertTrustedSender(event: IpcMainInvokeEvent): void {
  if (!mainWindow || event.sender !== mainWindow.webContents) {
    throw new Error("Untrusted IPC sender");
  }
  const expectedUrl = pathToFileURL(path.join(__dirname, "../renderer/index.html")).toString();
  if (!event.senderFrame || event.senderFrame.url !== expectedUrl) {
    throw new Error("Untrusted IPC origin");
  }
}

function parseStartConfig(value: unknown): StartGatewayConfig {
  if (
    typeof value !== "object" ||
    value === null ||
    !("remoteUrl" in value) ||
    !("requireProductionOs" in value) ||
    typeof value.remoteUrl !== "string" ||
    typeof value.requireProductionOs !== "boolean"
  ) {
    throw new Error("Invalid gateway configuration");
  }
  return {
    remoteUrl: value.remoteUrl,
    requireProductionOs: value.requireProductionOs,
  };
}

function registerIpc(): void {
  ipcMain.handle(IPC_CHANNELS.getState, (event) => {
    assertTrustedSender(event);
    return sidecar.getState();
  });
  ipcMain.handle(IPC_CHANNELS.start, async (event, value: unknown) => {
    assertTrustedSender(event);
    lastStartConfig = parseStartConfig(value);
    return sidecar.start(lastStartConfig);
  });
  ipcMain.handle(IPC_CHANNELS.stop, async (event) => {
    assertTrustedSender(event);
    return sidecar.stop();
  });
  ipcMain.handle(IPC_CHANNELS.listReceipts, async (event) => {
    assertTrustedSender(event);
    return sidecar.listReceipts();
  });
  ipcMain.handle(IPC_CHANNELS.copyText, (event, text: unknown) => {
    assertTrustedSender(event);
    if (typeof text !== "string" || text.length === 0 || text.length > 4_096) {
      throw new Error("Invalid clipboard text");
    }
    clipboard.writeText(text);
  });
}

app.whenReady().then(() => {
  Menu.setApplicationMenu(null);
  registerIpc();
  mainWindow = createWindow();
  tray = createTray();
  updateTray(sidecar.getState());
  unsubscribeState = sidecar.subscribe((state) => {
    updateTray(state);
    if (mainWindow && !mainWindow.isDestroyed()) {
      mainWindow.webContents.send(IPC_CHANNELS.stateChanged, state);
    }
  });
});

app.on("activate", () => {
  showMainWindow();
});

app.on("before-quit", (event) => {
  if (quitting) {
    return;
  }
  event.preventDefault();
  quitting = true;
  unsubscribeState?.();
  void sidecar.stop().finally(() => {
    tray?.destroy();
    tray = undefined;
    app.quit();
  });
});

app.on("window-all-closed", () => undefined);
