/**
 * pi-provider-phala-cloud — Phala Cloud branded distribution of the
 * vendor-neutral private-ai-gateway (ACI) Pi provider.
 *
 * This package is a thin skin: it imports the core `@phala/pi-provider-aci` and
 * registers it with the Phala Cloud identity (provider id, endpoint, env vars,
 * fallback catalog, OAuth login). All protocol logic — attestation, TLS SPKI
 * pinning, model discovery — lives in the core.
 *
 * Usage:
 *   pi install npm:pi-provider-phala-cloud
 *   # /login phala (or set PHALA_LLM_API_KEY), then /model phala/<model-id>
 */
import type { OAuthCredentials, OAuthLoginCallbacks } from "@earendil-works/pi-ai";
import { createProvider } from "@phala/pi-provider-aci";

// Phala Cloud (teahouse) API base for account-level endpoints: the OAuth
// device authorization flow and the LLM-key self lookup live here, not on
// the inference gateway.
const DEFAULT_CLOUD_API_URL = "https://cloud-api.phala.com";

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, Math.max(0, ms)));
}

export function getCloudApiBase(): string {
  const value = process.env.PHALA_CLOUD_API_BASE_URL || DEFAULT_CLOUD_API_URL;
  return value.trim().replace(/\/+$/, "") || DEFAULT_CLOUD_API_URL;
}

interface DeviceCodeResponse {
  device_code: string;
  user_code: string;
  verification_uri: string;
  verification_uri_complete?: string;
  expires_in: number;
  interval: number;
}

interface DeviceTokenResponse {
  access_token: string;
  expires_in?: number | null;
  redpill_key_id?: number | null;
}

interface PrivateAiSelfResponse {
  user?: { username?: string };
  workspace?: { name?: string; slug?: string | null };
  credits?: { balance?: string; granted_balance?: string };
}

// RFC 8628 device authorization against Phala Cloud. On approval the consume
// step (scope "redpill:api-key") issues a Redpill LLM virtual key — no phak_
// cloud token is created. The key does not expire and cannot be refreshed, so
// `expires` is set far in the future and refreshToken() always throws.
async function loginPhalaDeviceFlow(callbacks: OAuthLoginCallbacks): Promise<OAuthCredentials> {
  const cloudApi = getCloudApiBase();
  const codeRes = await fetch(`${cloudApi}/api/v1/auth/device/code`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ client_id: "pi", scope: "redpill:api-key" }),
    signal: callbacks.signal,
  });
  if (!codeRes.ok) {
    throw new Error(`Device authorization request failed: ${await codeRes.text()}`);
  }
  const code = (await codeRes.json()) as DeviceCodeResponse;

  callbacks.onDeviceCode({
    userCode: code.user_code,
    verificationUri: code.verification_uri_complete ?? code.verification_uri,
    intervalSeconds: code.interval,
    expiresInSeconds: code.expires_in,
  });

  const deadline = Date.now() + code.expires_in * 1000;
  let token: DeviceTokenResponse | undefined;
  // RFC 8628 §3.4: poll at the server-provided interval, and back off on
  // slow_down. A loop without this would hammer the token endpoint.
  let intervalMs = Math.max(Number(code.interval) || 5, 1) * 1000;
  while (Date.now() < deadline) {
    if (callbacks.signal?.aborted) throw new Error("Login cancelled");
    callbacks.onProgress?.("Waiting for authorization...");
    const tokenRes = await fetch(`${cloudApi}/api/v1/auth/device/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        device_code: code.device_code,
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
      }),
      signal: callbacks.signal,
    });
    if (tokenRes.ok) {
      token = (await tokenRes.json()) as DeviceTokenResponse;
      break;
    }
    const body = (await tokenRes.json().catch(() => undefined)) as
      | { detail?: { error?: string; error_description?: string } | string }
      | undefined;
    const detail = body?.detail;
    const errorCode = typeof detail === "object" && detail ? detail.error : undefined;
    if (errorCode === "authorization_pending") {
      await sleep(Math.min(intervalMs, deadline - Date.now()));
      continue;
    }
    if (errorCode === "slow_down") {
      // RFC 8628 §3.5: increase the polling interval.
      intervalMs = Math.min(Math.max(intervalMs * 2, 5000), 30000);
      await sleep(intervalMs);
      continue;
    }
    const description =
      (typeof detail === "object" && detail ? detail.error_description : undefined) ??
      (typeof detail === "string" ? detail : undefined) ??
      `HTTP ${tokenRes.status}`;
    throw new Error(`Device authorization failed: ${description}`);
  }
  if (!token) throw new Error("Device authorization expired");

  const credentials: OAuthCredentials = {
    refresh: "",
    access: token.access_token,
    expires: Date.now() + 100 * 365 * 24 * 60 * 60 * 1000,
  };
  if (typeof token.redpill_key_id === "number") {
    credentials.redpill_key_id = token.redpill_key_id;
  }

  // Best-effort display metadata from the LLM-key self endpoint.
  try {
    const selfRes = await fetch(`${cloudApi}/api/v1/private_ai/self`, {
      headers: { Authorization: `Bearer ${token.access_token}` },
    });
    if (selfRes.ok) {
      const self = (await selfRes.json()) as PrivateAiSelfResponse;
      if (self.user?.username) credentials.username = self.user.username;
      if (self.workspace?.slug) credentials.workspace_slug = self.workspace.slug;
      if (self.workspace?.name) credentials.workspace_name = self.workspace.name;
    }
  } catch {
    // Metadata is display-only; login still succeeds without it.
  }
  return credentials;
}

export default createProvider({
  providerId: "phala",
  label: "Phala Cloud",
  defaultBaseUrl: "https://inference.phala.com/v1",
  apiKeyEnv: "PHALA_LLM_API_KEY",
  envPrefix: "PHALA",
  footerKey: "phala",
  logPrefix: "[phala]",
  baseUrlAliases: ["PHALA_CLOUD_API_PREFIX", "PHALA_BASE_URL", "PHALA_CLOUD_BASE_URL"],
  fallbackModels: [
    {
      id: "phala/qwen3.5-27b",
      name: "Phala Qwen3.5 27B",
      reasoning: true,
      input: ["text"],
      cost: { input: 0.3, output: 2.4, cacheRead: 0, cacheWrite: 0 },
      contextWindow: 262000,
      maxTokens: 8192,
    },
  ],
  oauth: {
    name: "Phala Cloud",
    login: loginPhalaDeviceFlow,
    // Redpill LLM keys do not expire and have no rotation endpoint; a dead
    // key surfaces as a 401 and the user re-runs /login to mint a new one.
    refreshToken: () => {
      throw new Error("Phala LLM keys cannot be refreshed; run /login phala again");
    },
    getApiKey: (credentials) => credentials.access,
  },
});

export { createProvider } from "@phala/pi-provider-aci";
export { PROVIDER_VERSION } from "@phala/pi-provider-aci";