import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { open, save } from "@tauri-apps/plugin-dialog";

import type { DistributionCapabilities, ProfileBackup, UiMethod } from "../../shared/contracts";
import { createDesktopApi, type UiPlatform, type UiTransport } from "./create-api";

declare global {
  interface Window {
    __PAP_DISTRIBUTION__?: DistributionCapabilities;
  }
}

const transport: UiTransport = {
  call: (method, params = {}) => invoke(commandName(method), params),
  subscribe,
};

const platform: UiPlatform = {
  showEditMenu: (editable) => invoke("show_edit_menu", { editable }),
  getAppVersion: getVersion,
  setUpdateChannel: (channel) => invoke("set_update_channel", { channel }),
  prepareUpdate: () => invoke("prepare_update"),
  restartToUpdate: () => invoke("restart_to_update"),
  getCliRegistration: () => invoke("get_cli_registration"),
  setCliRegistration: (installed) => invoke("set_cli_registration", { installed }),
  stopAllAndQuit: () => invoke("stop_all_and_quit"),
  copyText: (text) => invoke("copy_text", { text }),
  selectProfileBackup: async () => {
    const path = await open({
      title: "Import Profile Configurations",
      multiple: false,
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    return path ? invoke<ProfileBackup>("read_profile_backup", { path }) : null;
  },
  saveProfileExport: async () => {
    const path = await save({
      title: "Export Profiles to a New File (No Keys)",
      defaultPath: "private-ai-proxy-profiles.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (path) await invoke("export_profiles", { path });
  },
  saveDiagnosticsExport: async () => {
    const path = await save({
      title: "Export Redacted Diagnostics to a New File",
      defaultPath: "private-ai-proxy-diagnostics.json",
      filters: [{ name: "JSON", extensions: ["json"] }],
    });
    if (path) await invoke("export_diagnostics", { path });
  },
  requestNotificationPermission: () => invoke("request_notification_permission"),
  openNotificationSettings: () => invoke("open_notification_settings"),
  mainWindowReady: () => invoke("main_window_ready"),
  openAboutLink: (target) => invoke("open_about_link", { target }),
  openWebUi: () => invoke("open_web_ui"),
  openAgentWebsite: (agentId) => invoke("open_agent_website", { agentId }),
  openApiKeyPage: (provider) => invoke("open_api_key_page", { provider }),
  presentAccountLogin: () => undefined,
  openOrganization: (organizationSlug) => invoke("open_organization", { organizationSlug }),
  openTopUp: (provider, scopeSlug) => invoke("open_top_up", { provider, scopeSlug }),
};

/** Tauri command names are the snake_case Rust function names of the shared UI methods. */
function commandName(method: UiMethod): string {
  return method.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
}

function subscribe<T>(event: string, listener: (payload: T) => void): () => void {
  let disposed = false;
  let unlisten: (() => void) | undefined;
  try {
    void listen<T>(event, (received) => {
      if (!disposed) listener(received.payload);
    }, {
      target: { kind: "WebviewWindow", label: getCurrentWebviewWindow().label },
    }).then(
      (nextUnlisten) => {
        if (disposed) nextUnlisten();
        else unlisten = nextUnlisten;
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

export async function createBackend() {
  const distributionCapabilities = window.__PAP_DISTRIBUTION__;
  delete window.__PAP_DISTRIBUTION__;
  if (!distributionCapabilities) throw new Error("Distribution capabilities were not initialized");
  return {
    desktopApi: createDesktopApi(transport, platform),
    distributionCapabilities,
    signOut: undefined,
  };
}
