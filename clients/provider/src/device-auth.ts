import type { AciFetch } from "@phala/aci-verifier/runtime";

const DEVICE_GRANT_TYPE = "urn:ietf:params:oauth:grant-type:device_code";

export interface PhalaCloudDeviceAuthorizationOptions {
  baseURL: string;
  clientId: string;
  fetch?: AciFetch;
  signal?: AbortSignal;
}

export interface PhalaCloudDeviceAuthorizationPollOptions {
  signal?: AbortSignal;
  onProgress?: (message: string) => void;
}

export interface PhalaCloudApiKey {
  accessToken: string;
  expiresIn?: number;
  keyId?: number;
}

export interface PhalaCloudDeviceAuthorization {
  userCode: string;
  verificationURI: string;
  expiresIn: number;
  interval: number;
  poll(options?: PhalaCloudDeviceAuthorizationPollOptions): Promise<PhalaCloudApiKey>;
}

interface DeviceCodeResponse {
  device_code: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete?: string;
  expires_in: number;
  interval: number;
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} returned an invalid response`);
  }
  return value as Record<string, unknown>;
}

function requiredString(value: unknown, field: string): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new Error(`Device authorization response is missing ${field}`);
  }
  return value;
}

function positiveNumber(value: unknown, field: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
    throw new Error(`Device authorization response has invalid ${field}`);
  }
  return value;
}

function isLoopback(hostname: string): boolean {
  return hostname === "localhost" || hostname === "127.0.0.1" || hostname === "[::1]";
}

function httpURL(value: unknown, field: string): URL {
  const text = requiredString(value, field);
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    throw new Error(`Device authorization response has invalid ${field}`);
  }
  if (url.protocol !== "https:" && !(url.protocol === "http:" && isLoopback(url.hostname))) {
    throw new Error(`Device authorization response has insecure ${field}`);
  }
  return url;
}

export function phalaCloudEndpoint(baseURL: string, path: string): URL {
  const root = httpURL(baseURL, "baseURL");
  root.pathname = `${root.pathname.replace(/\/$/, "")}${path}`;
  root.search = "";
  root.hash = "";
  return root;
}

function parseDeviceCode(value: unknown): DeviceCodeResponse {
  const data = record(value, "Device authorization endpoint");
  return {
    device_code: requiredString(data.device_code, "device_code"),
    user_code: requiredString(data.user_code, "user_code"),
    verification_uri: httpURL(data.verification_uri, "verification_uri").href,
    ...(data.verification_uri_complete === undefined
      ? {}
      : {
          verification_uri_complete: httpURL(
            data.verification_uri_complete,
            "verification_uri_complete",
          ).href,
        }),
    expires_in: positiveNumber(data.expires_in, "expires_in"),
    interval: positiveNumber(data.interval, "interval"),
  };
}

function parseDeviceToken(value: unknown): PhalaCloudApiKey {
  const data = record(value, "Device token endpoint");
  const expiresIn = data.expires_in;
  if (
    expiresIn !== undefined &&
    expiresIn !== null &&
    (typeof expiresIn !== "number" || !Number.isFinite(expiresIn) || expiresIn <= 0)
  ) {
    throw new Error("Device token response has invalid expires_in");
  }
  const keyId = data.redpill_key_id;
  if (
    keyId !== undefined &&
    keyId !== null &&
    (typeof keyId !== "number" || !Number.isInteger(keyId) || keyId < 0)
  ) {
    throw new Error("Device token response has invalid redpill_key_id");
  }
  return {
    accessToken: requiredString(data.access_token, "access_token"),
    ...(typeof expiresIn === "number" ? { expiresIn } : {}),
    ...(typeof keyId === "number" ? { keyId } : {}),
  };
}

function deviceError(value: unknown): { code?: string; description?: string } {
  if (value === null || typeof value !== "object" || Array.isArray(value)) return {};
  const root = value as Record<string, unknown>;
  const detail = root.detail;
  const fields =
    detail !== null && typeof detail === "object" && !Array.isArray(detail)
      ? (detail as Record<string, unknown>)
      : root;
  const code = typeof fields.error === "string" ? fields.error : root.error;
  const description =
    typeof fields.error_description === "string"
      ? fields.error_description
      : typeof detail === "string"
        ? detail
        : root.error_description;
  return {
    ...(typeof code === "string" ? { code } : {}),
    ...(typeof description === "string" ? { description } : {}),
  };
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  if (signal?.aborted) return Promise.reject(new Error("Device authorization cancelled"));
  return new Promise((resolve, reject) => {
    const done = () => {
      signal?.removeEventListener("abort", abort);
      resolve();
    };
    const abort = () => {
      clearTimeout(timeout);
      reject(new Error("Device authorization cancelled"));
    };
    const timeout = setTimeout(done, Math.max(0, ms));
    signal?.addEventListener("abort", abort, { once: true });
  });
}

export async function startPhalaCloudDeviceAuthorization({
  baseURL,
  clientId,
  fetch = globalThis.fetch,
  signal,
}: PhalaCloudDeviceAuthorizationOptions): Promise<PhalaCloudDeviceAuthorization> {
  const codeURL = phalaCloudEndpoint(baseURL, "/api/v1/auth/device/code");
  const tokenURL = phalaCloudEndpoint(baseURL, "/api/v1/auth/device/token");
  const response = await fetch(codeURL, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ client_id: clientId, scope: "redpill:api-key" }),
    signal,
  });
  if (!response.ok) {
    throw new Error(`Device authorization request failed with HTTP ${response.status}`);
  }
  const code = parseDeviceCode(await response.json());
  const expiresIn = code.expires_in;
  const deadline = Date.now() + expiresIn * 1000;
  const initialInterval = code.interval * 1000;

  return {
    userCode: code.user_code,
    verificationURI: code.verification_uri_complete ?? code.verification_uri,
    expiresIn,
    interval: code.interval,
    async poll(options = {}) {
      const pollSignal = options.signal ?? signal;
      let interval = initialInterval;
      while (Date.now() < deadline) {
        if (pollSignal?.aborted) throw new Error("Device authorization cancelled");
        options.onProgress?.("Waiting for authorization...");
        await sleep(Math.min(interval, deadline - Date.now()), pollSignal);
        if (Date.now() >= deadline) break;
        const tokenResponse = await fetch(tokenURL, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({
            device_code: code.device_code,
            client_id: clientId,
            grant_type: DEVICE_GRANT_TYPE,
          }),
          signal: pollSignal,
        });
        if (tokenResponse.ok) return parseDeviceToken(await tokenResponse.json());

        const error = deviceError(await tokenResponse.json().catch(() => undefined));
        if (error.code === "authorization_pending") {
          continue;
        }
        if (error.code === "slow_down") {
          interval = Math.min(interval + 5000, 30000);
          continue;
        }
        throw new Error(
          `Device authorization failed: ${error.description ?? `HTTP ${tokenResponse.status}`}`,
        );
      }
      throw new Error("Device authorization expired");
    },
  };
}
