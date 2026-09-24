import type {
  Appearance,
  CliRegistration,
  DistributionCapabilities,
  AppState,
  ProfileBackup,
  ServiceProvider,
  UiMethod,
  UpdateInfo,
  UpdateNotice,
  WebBootstrap,
} from "../../shared/contracts";
import { showBrowserDialog } from "../components/browser-dialog";
import { showSignIn } from "../components/sign-in";
import { createDesktopApi, type UiPlatform, type UiTransport } from "./create-api";

type EventListener = (payload: never) => void;

/** Why the last session ended, shown once on the sign-in page after a reload. */
const noticeKey = "private-ai-proxy-web-notice";
const sessionEnded = "Your web UI session ended or expired. Sign in again.";
const signedOut = "You signed out. Sign in again to continue.";
const listeners = new Map<string, Set<EventListener>>();
let ended = false;

export async function createBackend(): Promise<{
  desktopApi: ReturnType<typeof createDesktopApi>;
  distributionCapabilities: DistributionCapabilities;
  initialAppState: AppState | undefined;
  initialAppearance: Appearance | undefined;
  signOut: (() => Promise<void>) | undefined;
}> {
  const bootstrap = await signIn();
  const transport: UiTransport = { call: rpc, subscribe };
  readEvents();
  return {
    desktopApi: createDesktopApi(transport, createPlatform(bootstrap)),
    distributionCapabilities: bootstrap.distribution,
    initialAppState: undefined,
    initialAppearance: undefined,
    signOut,
  };
}

function createPlatform(bootstrap: WebBootstrap): UiPlatform {
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
    // The backend's own installation owns updates; the browser only announces them.
    prepareUpdate: async (): Promise<UpdateInfo> => {
      const notice = await rpc<UpdateNotice>("getUpdateNotice");
      return {
        enabled: false,
        systemManaged: true,
        currentVersion: notice.currentVersion,
        channel: notice.channel,
        version: notice.version,
        channelPublished: notice.channelPublished,
        upgradeCommands: notice.commands,
        downloadUrl: notice.downloadUrl,
      };
    },
    restartToUpdate: async () => undefined,
    getCliRegistration: async () => registration,
    setCliRegistration: async () => registration,
    stopAllAndQuit: async () => undefined,
    copyText: async (text) => {
      // Absent outside secure contexts, such as plain HTTP on a network address.
      if (!window.isSecureContext) throw new Error("Copying needs 127.0.0.1 or HTTPS in this browser. Select and copy the text instead.");
      await navigator.clipboard.writeText(text);
    },
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
      const state = await rpc<AppState>("getState");
      emit("pap://dialog-open", {
        kind: kind === "setup-profile" ? "profile-editor" : kind,
        state,
        repair: options?.repair ?? false,
        recordId: options?.recordId,
        profileId: options?.profileId,
        startAfterSave: kind === "setup-profile",
      });
    },
    closeNativeDialog: async () => emit("pap://dialog-dismissed", undefined),
    nativeDialogReady: async () => undefined,
    mainWindowReady: async () => undefined,
    openWebUi: async () => {
      throw new Error("The web UI is already open in this browser");
    },
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
    presentAccountLogin: (login) => {
      // The login sheet keeps a manual link, so a blocked or rejected tab must not fail the login.
      try {
        openAllowed(login.url);
      } catch {
        return;
      }
    },
    openOrganization: async (organizationSlug) => openAllowed(
      await rpc<string>("getOrganizationUrl", { organizationSlug }),
    ),
    openTopUp: async (provider, scopeSlug) => openAllowed(
      await rpc<string>("getTopUpUrl", { provider, scopeSlug }),
    ),
  };
}

/**
 * Loads the bootstrap with this browser's session cookie, showing the password
 * sign-in page first when there is no live session.
 */
async function signIn(): Promise<WebBootstrap> {
  const response = await fetch("/api/bootstrap", { cache: "no-store", credentials: "same-origin" });
  // Signed-in requests are never throttled, so 429 also means there is no session.
  if (response.status !== 401 && response.status !== 429) return read<WebBootstrap>(response);
  const notice = sessionStorage.getItem(noticeKey) ?? undefined;
  sessionStorage.removeItem(noticeKey);
  await showSignIn(notice, openSession);
  return request<WebBootstrap>("/api/bootstrap", { method: "GET" });
}

/** Exchanges the web UI password for an `HttpOnly` session cookie. */
async function openSession(password: string): Promise<void> {
  const response = await fetch("/api/session", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ password }),
    cache: "no-store",
    credentials: "same-origin",
  });
  if (response.ok) return;
  const payload: unknown = await response.json().catch(() => undefined);
  throw new Error(isErrorPayload(payload) ? payload.error.message : "Sign-in failed. Try again.");
}

/** Ends this browser's session on the server, then returns to the sign-in page. */
async function signOut(): Promise<void> {
  await request<undefined>("/api/session", { method: "DELETE" });
  restartSignIn(signedOut);
}

/** The session cannot be recovered in place; reload into the sign-in page. */
function endSession(text: string): never {
  restartSignIn(text);
  throw new Error(text);
}

function restartSignIn(text: string): void {
  ended = true;
  sessionStorage.setItem(noticeKey, text);
  window.location.reload();
}

async function rpc<T>(method: UiMethod, params: Record<string, unknown> = {}): Promise<T> {
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
  return read<T>(await fetch(path, { ...init, cache: "no-store", credentials: "same-origin" }));
}

async function read<T>(response: Response): Promise<T> {
  const payload: unknown = await response.json().catch(() => undefined);
  if (response.status === 401) endSession(sessionEnded);
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

/** The server sends a full state snapshot on every connection, so reconnecting loses nothing. */
function readEvents(): void {
  const source = new EventSource("/api/events");
  source.addEventListener("message", (message) => {
    let decoded: unknown;
    try {
      decoded = JSON.parse(message.data);
    } catch {
      return;
    }
    if (isWebEvent(decoded)) emit(decoded.event, decoded.payload);
  });
  // EventSource retries dropped connections itself but stops at an error
  // response, such as after the session ended.
  source.addEventListener("error", () => {
    if (source.readyState === EventSource.CLOSED && !ended) window.setTimeout(() => void resumeEvents(), 1_000);
  });
}

/** Reopens the stream while the session lives; an ended session returns to sign-in. */
async function resumeEvents(): Promise<void> {
  try {
    await request("/api/bootstrap", { method: "GET" });
  } catch {
    if (!ended) window.setTimeout(() => void resumeEvents(), 5_000);
    return;
  }
  readEvents();
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
