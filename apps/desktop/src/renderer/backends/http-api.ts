import type {
  Appearance,
  CliRegistration,
  DistributionCapabilities,
  GatewayState,
  ProfileBackup,
  ServiceProvider,
  UpdateInfo,
} from "../../shared/contracts";
import { showBrowserDialog } from "../components/browser-dialog";
import { createDesktopApi, type UiMethod, type UiPlatform, type UiTransport } from "./create-api";

type EventListener = (payload: never) => void;
type Bootstrap = { version: string; distribution: DistributionCapabilities };

const tokenKey = "private-ai-proxy-web-token";
const token = consumeToken();
const listeners = new Map<string, Set<EventListener>>();

export async function createBackend(): Promise<{
  desktopApi: ReturnType<typeof createDesktopApi>;
  distributionCapabilities: DistributionCapabilities;
  initialGatewayState: GatewayState | undefined;
  initialAppearance: Appearance | undefined;
}> {
  const bootstrap = await request<Bootstrap>("/api/bootstrap", { method: "GET" });
  const transport: UiTransport = { call: rpc, subscribe };
  void readEvents();
  return {
    desktopApi: createDesktopApi(transport, createPlatform(bootstrap)),
    distributionCapabilities: bootstrap.distribution,
    initialGatewayState: undefined,
    initialAppearance: undefined,
  };
}

function createPlatform(bootstrap: Bootstrap): UiPlatform {
  const registration: CliRegistration = {
    executable: "private-ai-proxy",
    commandPath: "pap",
    installed: true,
    onPath: true,
  };
  return {
    showEditMenu: async () => undefined,
    getAppVersion: async () => bootstrap.version,
    setUpdateChannel: async (channel) => channel,
    prepareUpdate: async (): Promise<UpdateInfo> => ({
      enabled: false,
      systemManaged: true,
      currentVersion: bootstrap.version,
      channel: "stable",
      channelPublished: false,
    }),
    restartToUpdate: async () => undefined,
    getCliRegistration: async () => registration,
    setCliRegistration: async () => registration,
    stopAllAndQuit: async () => undefined,
    copyText: async (text) => navigator.clipboard.writeText(text),
    selectProfileBackup,
    saveProfileExport: async () => download(
      "private-ai-proxy-profiles.json",
      await rpc<string>("exportProfilesContent"),
    ),
    saveDiagnosticsExport: async () => download(
      "private-ai-proxy-diagnostics.json",
      await rpc<string>("exportDiagnosticsContent"),
    ),
    readProfileBackup: async () => {
      throw new Error("Browser imports use a local file picker");
    },
    exportProfiles: async () => {
      throw new Error("Browser exports download directly");
    },
    exportDiagnostics: async () => {
      throw new Error("Browser exports download directly");
    },
    requestNotificationPermission: async () => ({ permission: "unsupported", alertsEnabled: false }),
    openNotificationSettings: async () => undefined,
    openNativeDialog: async (kind, options) => {
      const state = await rpc<GatewayState>("getState");
      emit("gateway://dialog-open", {
        kind: kind === "setup-profile" ? "profile-editor" : kind,
        state,
        repair: options?.repair ?? false,
        recordId: options?.recordId,
        profileId: options?.profileId,
        startAfterSave: kind === "setup-profile",
      });
    },
    closeNativeDialog: async () => emit("gateway://dialog-dismissed", undefined),
    nativeDialogReady: async () => undefined,
    mainWindowReady: async () => undefined,
    openAboutLink: async (target) => openAllowed({
      documentation: "https://github.com/Dstack-TEE/private-ai-gateway#readme",
      github: "https://github.com/Dstack-TEE/private-ai-gateway",
      aci: "https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/attested-confidential-inference.md",
    }[target]),
    openAgentWebsite: async (agentId) => openAllowed(agentWebsites[agentId]),
    openApiKeyPage: async (provider) => openAllowed(apiKeyPages[provider]),
    confirm: (options) => showBrowserDialog({
      ...options,
      cancelLabel: options.cancelLabel ?? "Cancel",
    }),
    showErrorAlert: async (title, message) => {
      await showBrowserDialog({ title, message, confirmLabel: "OK" });
    },
    presentAccountLogin: (login) => openAllowed(login.url),
    openOrganization: async (organizationSlug) => openAllowed(
      await rpc<string>("getOrganizationUrl", { organizationSlug }),
    ),
    openTopUp: async (provider, scopeSlug) => openAllowed(
      await rpc<string>("getTopUpUrl", { provider, scopeSlug }),
    ),
  };
}

function consumeToken(): string {
  const fragment = new URLSearchParams(window.location.hash.replace(/^#/, ""));
  const supplied = fragment.get("token");
  if (supplied) {
    sessionStorage.setItem(tokenKey, supplied);
    history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
  }
  const stored = supplied ?? sessionStorage.getItem(tokenKey);
  if (!stored || !/^[A-Za-z0-9_-]{43}$/.test(stored)) {
    throw new Error("This web UI link is missing or has an invalid session token. Run `pap ui` again.");
  }
  return stored;
}

async function rpc<T>(method: UiMethod | "exportProfilesContent" | "exportDiagnosticsContent" | "getOrganizationUrl" | "getTopUpUrl", params: Record<string, unknown> = {}): Promise<T> {
  const response = await request<{ result?: T; error?: { message?: string } }>(
    `/api/rpc/${encodeURIComponent(method)}`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(params),
    },
  );
  if (response.error) throw new Error(response.error.message || "Management request failed");
  return response.result as T;
}

async function request<T>(path: string, init: RequestInit): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set("Authorization", `Bearer ${token}`);
  const response = await fetch(path, {
    ...init,
    headers,
    cache: "no-store",
    credentials: "same-origin",
  });
  const payload: unknown = await response.json().catch(() => undefined);
  if (!response.ok) {
    const message = isErrorPayload(payload) ? payload.error.message : "Web UI request failed";
    throw new Error(message);
  }
  return payload as T;
}

function isErrorPayload(value: unknown): value is { error: { message: string } } {
  if (!value || typeof value !== "object" || !("error" in value)) return false;
  const error = value.error;
  return Boolean(
    error && typeof error === "object" && "message" in error && typeof error.message === "string",
  );
}

function subscribe<T>(event: string, listener: (payload: T) => void): () => void {
  const wrapped: EventListener = listener as EventListener;
  const current = listeners.get(event) ?? new Set<EventListener>();
  current.add(wrapped);
  listeners.set(event, current);
  return () => current.delete(wrapped);
}

function emit(event: string, payload: unknown): void {
  for (const listener of listeners.get(event) ?? []) listener(payload as never);
}

async function readEvents(): Promise<void> {
  try {
    const response = await fetch("/api/events", {
      headers: { Authorization: `Bearer ${token}` },
      cache: "no-store",
      credentials: "same-origin",
    });
    if (!response.ok || !response.body) throw new Error("Event stream unavailable");
    const reader = response.body.pipeThrough(new TextDecoderStream()).getReader();
    let buffer = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += value;
      const blocks = buffer.split("\n\n");
      buffer = blocks.pop() ?? "";
      for (const block of blocks) {
        const data = block.split("\n")
          .filter((line) => line.startsWith("data:"))
          .map((line) => line.slice(5).trimStart())
          .join("\n");
        if (!data) continue;
        const decoded: unknown = JSON.parse(data);
        if (isWebEvent(decoded)) emit(decoded.event, decoded.payload);
      }
    }
  } catch {
    window.setTimeout(() => void readEvents(), 1_000);
  }
}

function isWebEvent(value: unknown): value is { event: string; payload: unknown } {
  return Boolean(
    value && typeof value === "object" && "event" in value
      && typeof value.event === "string" && "payload" in value,
  );
}

function selectProfileBackup(): Promise<ProfileBackup | null> {
  return new Promise((resolve, reject) => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "application/json,.json";
    input.addEventListener("change", () => {
      const file = input.files?.[0];
      if (!file) {
        resolve(null);
        return;
      }
      if (file.size > 256 * 1024) {
        reject(new Error("Profile configuration file is too large"));
        return;
      }
      void parseProfileBackup(file).then(resolve, reject);
    }, { once: true });
    input.click();
  });
}

async function parseProfileBackup(file: File): Promise<ProfileBackup> {
  let parsed: unknown;
  try {
    parsed = JSON.parse(await file.text());
  } catch {
    throw new Error("Could not read the profile configuration file");
  }
  if (!isProfileBackup(parsed)) throw new Error("The profile configuration file is invalid");
  return parsed;
}

function isProfileBackup(value: unknown): value is ProfileBackup {
  if (!value || typeof value !== "object" || !("version" in value) || value.version !== 1
    || !("profiles" in value) || !Array.isArray(value.profiles)) return false;
  return value.profiles.every((profile: unknown) => Boolean(
    profile && typeof profile === "object"
      && "name" in profile && typeof profile.name === "string"
      && "provider" in profile
      && (profile.provider === "phala" || profile.provider === "redpill" || profile.provider === "custom")
      && "remoteUrl" in profile && typeof profile.remoteUrl === "string",
  ));
}

function download(name: string, content: string): void {
  const url = URL.createObjectURL(new Blob([content], { type: "application/json" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
}

function openAllowed(url: string | undefined): void {
  if (!url) throw new Error("This link is unavailable");
  const parsed = new URL(url);
  if (parsed.protocol !== "https:") throw new Error("Only secure external links are allowed");
  window.open(parsed.href, "_blank", "noopener,noreferrer");
}

const agentWebsites: Record<string, string> = {
  codex: "https://developers.openai.com/codex/cli/",
  "claude-code": "https://code.claude.com",
  opencode: "https://opencode.ai",
  pi: "https://pi.dev",
  hermes: "https://hermes-agent.nousresearch.com",
  openclaw: "https://openclaw.ai",
  "oh-my-pi": "https://omp.sh",
};
const apiKeyPages: Partial<Record<ServiceProvider, string>> = {
  phala: "https://cloud.phala.com/dashboard",
  redpill: "https://www.redpill.ai/dashboard",
};
