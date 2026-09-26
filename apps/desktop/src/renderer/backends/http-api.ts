import {
  ABOUT_LINKS,
  AGENT_WEBSITES,
  API_KEY_PAGES,
  SERVICE_PROVIDERS,
  WEB_DISTRIBUTION,
  type ProfileBackup,
  type UiMethod,
  type UpdateInfo,
  type UpdateNotice,
  type WebBootstrap,
} from "../../shared/contracts";
import { createDesktopApi, type Backend, type UiPlatform, type UiTransport, type WebSession } from "./create-api";

type EventListener = (payload: never) => void;

const sessionEnded = "Your web UI session ended or expired. Sign in again.";
const signedOut = "You signed out. Sign in again to continue.";
const listeners = new Map<string, Set<EventListener>>();
const endListeners = new Set<(notice: string) => void>();
let events: EventSource | undefined;
/** A session was confirmed and has not ended since, so its end is announced once. */
let signedIn = false;

export function createBackend(): Backend {
  const transport: UiTransport = { call: rpc, subscribe };
  const session: WebSession = {
    check,
    signIn,
    signOut,
    onEnded: (listener) => {
      endListeners.add(listener);
      return () => endListeners.delete(listener);
    },
  };
  return {
    desktopApi: createDesktopApi(transport, platform),
    distributionCapabilities: WEB_DISTRIBUTION,
    session,
  };
}

const unavailable = async (): Promise<never> => {
  throw new Error("This is only available in the desktop app");
};

const platform: UiPlatform = {
  showEditMenu: async () => undefined,
  getAppVersion: async () => (await request<WebBootstrap>("/api/bootstrap", { method: "GET" })).version,
  setUpdateChannel: async (channel) => channel,
  // The backend's own installation owns updates; the browser only announces them.
  prepareUpdate: async (): Promise<UpdateInfo> => {
    const notice = await rpc<UpdateNotice>("get_update_notice");
    return {
      enabled: false,
      systemManaged: true,
      currentVersion: notice.currentVersion,
      channel: notice.channel,
      version: notice.version,
      upgradeCommands: notice.commands,
      downloadUrl: notice.downloadUrl,
    };
  },
  restartToUpdate: async () => undefined,
  getCliRegistration: unavailable,
  setCliRegistration: unavailable,
  stopAllAndQuit: async () => undefined,
  showConfirmation: unavailable,
  closeWindow: unavailable,
  quit: unavailable,
  copyText: async (text) => {
    // Absent outside secure contexts, such as plain HTTP on a network address.
    if (!window.isSecureContext) throw new Error("Copying needs 127.0.0.1 or HTTPS in this browser. Select and copy the text instead.");
    await navigator.clipboard.writeText(text);
  },
  selectProfileBackup,
  saveProfileExport: async () => download(
    "private-ai-proxy-profiles.json",
    await rpc<string>("export_profiles_content"),
  ),
  saveDiagnosticsExport: async () => download(
    "private-ai-proxy-diagnostics.json",
    await rpc<string>("export_diagnostics_content"),
  ),
  requestNotificationPermission: async () => ({ permission: "unsupported", alertsEnabled: false }),
  openNotificationSettings: async () => undefined,
  openWebUi: async () => {
    throw new Error("The web UI is already open in this browser");
  },
  openAboutLink: async (target) => openAllowed(ABOUT_LINKS[target]),
  openAgentWebsite: async (agentId) => openAllowed(AGENT_WEBSITES[agentId]),
  openApiKeyPage: async (provider) => openAllowed(API_KEY_PAGES[provider]),
  presentAccountLogin: (login) => {
    // The login sheet keeps a manual link, so a blocked or rejected tab must not fail the login.
    try {
      openAllowed(login.url);
    } catch {
      return;
    }
  },
  openOrganization: async (organizationSlug) => openAllowed(
    await rpc<string>("get_organization_url", { organizationSlug }),
  ),
  openTopUp: async (provider, scopeSlug) => openAllowed(
    await rpc<string>("get_top_up_url", { provider, scopeSlug }),
  ),
};

async function check(): Promise<boolean> {
  // Confirmed sessions are remembered until they end, so navigating costs no request.
  if (signedIn) return true;
  const response = await fetch("/api/bootstrap", { cache: "no-store", credentials: "same-origin" });
  // Signed-in requests are never throttled, so 429 also means there is no session.
  if (response.status === 401 || response.status === 429) return false;
  await read<WebBootstrap>(response);
  signedIn = true;
  events ??= readEvents();
  return true;
}

async function signIn(password: string): Promise<void> {
  const response = await fetch("/api/session", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ password }),
    cache: "no-store",
    credentials: "same-origin",
  });
  if (response.ok) return;
  const payload: unknown = await response.json().catch(() => undefined);
  throw errorFrom(payload, "Sign-in failed. Try again.");
}

/** Ends this browser's session on the server. */
async function signOut(): Promise<void> {
  await request<undefined>("/api/session", { method: "DELETE" });
  endSession(signedOut);
}

function endSession(notice: string): void {
  events?.close();
  events = undefined;
  if (!signedIn) return;
  signedIn = false;
  for (const listener of endListeners) listener(notice);
}

async function rpc<T>(method: UiMethod, params: Record<string, unknown> = {}): Promise<T> {
  const response = await request<{ result: T }>(
    `/api/rpc/${encodeURIComponent(method)}`,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(params),
    },
  );
  return response.result;
}

async function request<T>(path: string, init: RequestInit): Promise<T> {
  return read<T>(await fetch(path, { ...init, cache: "no-store", credentials: "same-origin" }));
}

/** Answers are `{"result": …}`, or `{"error": {code, message}}` with the status of its code. */
async function read<T>(response: Response): Promise<T> {
  const payload: unknown = await response.json().catch(() => undefined);
  if (response.status === 401) {
    endSession(sessionEnded);
    throw new Error(sessionEnded);
  }
  if (!response.ok) throw errorFrom(payload, "Web UI request failed");
  return payload as T;
}

function errorFrom(payload: unknown, fallback: string): Error {
  if (payload && typeof payload === "object" && "error" in payload) {
    const error = payload.error;
    if (error && typeof error === "object" && "message" in error && typeof error.message === "string") {
      return new Error(error.message);
    }
  }
  return new Error(fallback);
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
function readEvents(): EventSource {
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
    if (source.readyState === EventSource.CLOSED && events === source) window.setTimeout(() => void resumeEvents(source), 1_000);
  });
  return source;
}

/** Reopens the stream while the session lives; an ended session returns to sign-in. */
async function resumeEvents(closed: EventSource): Promise<void> {
  if (events !== closed) return;
  try {
    await request("/api/bootstrap", { method: "GET" });
  } catch {
    if (events === closed) window.setTimeout(() => void resumeEvents(closed), 5_000);
    return;
  }
  if (events === closed) events = readEvents();
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
      && "provider" in profile && typeof profile.provider === "string" && Object.hasOwn(SERVICE_PROVIDERS, profile.provider)
      && "remoteUrl" in profile && typeof profile.remoteUrl === "string",
  ));
}

function download(name: string, content: string): boolean {
  const url = URL.createObjectURL(new Blob([content], { type: "application/json" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
  return true;
}

function openAllowed(url: string | undefined): void {
  if (!url) throw new Error("This link is unavailable");
  const parsed = new URL(url);
  if (parsed.protocol !== "https:") throw new Error("Only secure external links are allowed");
  window.open(parsed.href, "_blank", "noopener,noreferrer");
}
