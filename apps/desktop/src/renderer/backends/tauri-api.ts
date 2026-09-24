import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { confirm, open, save } from "@tauri-apps/plugin-dialog";

import type {
  Appearance,
  DistributionCapabilities,
  AppState,
  ProfileBackup,
  UiMethod,
} from "../../shared/contracts";
import { createDesktopApi, type UiPlatform, type UiTransport } from "./create-api";

declare global {
  interface Window {
    __PAP_INITIAL_STATE__?: AppState;
    __PAP_INITIAL_APPEARANCE__?: Appearance;
    __PAP_DISTRIBUTION__?: DistributionCapabilities;
  }
}

const commandOverrides: Partial<Record<UiMethod, string>> = {
  getAccountDetails: "account_details",
  getAccountBalance: "account_balance",
  previewAgent: "preview_agent_connection",
  applyAgent: "apply_agent_connection",
};

const transport: UiTransport = {
  call: (method, params = {}) => invoke(commandOverrides[method] ?? snakeCase(method), params),
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
  readProfileBackup: (path) => invoke("read_profile_backup", { path }),
  exportProfiles: (path) => invoke("export_profiles", { path }),
  exportDiagnostics: (path) => invoke("export_diagnostics", { path }),
  requestNotificationPermission: () => invoke("request_notification_permission"),
  openNotificationSettings: () => invoke("open_notification_settings"),
  openNativeDialog: (kind, options) => invoke("open_native_dialog", {
    kind,
    repair: options?.repair ?? false,
    recordId: options?.recordId,
    profileId: options?.profileId,
  }),
  closeNativeDialog: () => invoke("close_native_dialog"),
  nativeDialogReady: () => invoke("native_dialog_ready"),
  mainWindowReady: () => invoke("main_window_ready"),
  openAboutLink: (target) => invoke("open_about_link", { target }),
  openAgentWebsite: (agentId) => invoke("open_agent_website", { agentId }),
  openApiKeyPage: (provider) => invoke("open_api_key_page", { provider }),
  confirm: (options) => confirm(options.message, {
    title: options.title,
    kind: "warning",
    okLabel: options.confirmLabel,
    cancelLabel: options.cancelLabel ?? "Cancel",
  }),
  showErrorAlert: (title, message) => invoke("show_error_alert", { title, message }),
  presentAccountLogin: () => undefined,
  openOrganization: (organizationSlug) => invoke("open_organization", { organizationSlug }),
  openTopUp: (provider, scopeSlug) => invoke("open_top_up", { provider, scopeSlug }),
};

function snakeCase(name: string): string {
  return name.replace(/[A-Z]/g, (letter) => `_${letter.toLowerCase()}`);
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
  const initialAppState = window.__PAP_INITIAL_STATE__;
  delete window.__PAP_INITIAL_STATE__;
  const initialAppearance = window.__PAP_INITIAL_APPEARANCE__;
  delete window.__PAP_INITIAL_APPEARANCE__;
  const distributionCapabilities = window.__PAP_DISTRIBUTION__;
  delete window.__PAP_DISTRIBUTION__;
  if (!distributionCapabilities) throw new Error("Distribution capabilities were not initialized");
  return {
    desktopApi: createDesktopApi(transport, platform),
    distributionCapabilities,
    initialAppearance,
    initialAppState,
    signOut: undefined,
  };
}
