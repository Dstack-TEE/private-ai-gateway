import { getVersion } from "@tauri-apps/api/app";
import { invoke as tauriInvoke, type InvokeArgs } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

import type { DistributionCapabilities } from "../../shared/contracts";
import { createDesktopApi, type Backend, type UiPlatform, type UiTransport } from "./create-api";

declare global {
  interface Window {
    __PAP_DISTRIBUTION__?: DistributionCapabilities;
  }
}

/** Commands reject with the serialized API error `{code, message}`; the renderer shows the message. */
async function invoke<T>(command: string, args?: InvokeArgs): Promise<T> {
  try {
    return await tauriInvoke<T>(command, args);
  } catch (error) {
    if (error && typeof error === "object" && "message" in error && typeof error.message === "string") {
      throw new Error(error.message);
    }
    throw error instanceof Error ? error : new Error(String(error));
  }
}

const transport: UiTransport = {
  call: (method, params = {}) => invoke(method, params),
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
  // The shell shows the system file panels and reads or writes the chosen file.
  selectProfileBackup: () => invoke("select_profile_backup"),
  saveProfileExport: () => invoke("export_profiles"),
  saveDiagnosticsExport: () => invoke("export_diagnostics"),
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

export function createBackend(): Backend {
  const distributionCapabilities = window.__PAP_DISTRIBUTION__;
  delete window.__PAP_DISTRIBUTION__;
  if (!distributionCapabilities) throw new Error("Distribution capabilities were not initialized");
  return {
    desktopApi: createDesktopApi(transport, platform),
    distributionCapabilities,
    session: undefined,
  };
}
