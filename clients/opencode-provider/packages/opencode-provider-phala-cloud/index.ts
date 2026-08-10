/**
 * opencode-provider-phala-cloud — Phala Cloud branded distribution of the
 * vendor-neutral private-ai-gateway (ACI) opencode provider.
 *
 * This package is a thin skin: it imports the core `@phala/opencode-provider-aci`
 * and registers it with the Phala Cloud identity (provider id, endpoint, env
 * vars, fallback catalog, OAuth device-flow login). All protocol logic —
 * attestation, TLS SPKI pinning, receipt verification, model discovery —
 * lives in the core.
 *
 * Usage:
 *   opencode.json: { "plugin": ["opencode-provider-phala-cloud"] }
 *   # opencode auth login (pick Phala Cloud), or set PHALA_LLM_API_KEY,
 *   # then select model phala/<model-id>
 */
import {
  type AciDeviceFlowStart,
  type AciOAuthCredentials,
  createProvider,
} from "@phala/opencode-provider-aci/core";

// Phala Cloud (teahouse) API base for account-level endpoints: the OAuth
// device authorization flow and the LLM-key self lookup live here, not on the
// inference gateway.
const DEFAULT_CLOUD_API_URL = "https://cloud-api.phala.com";

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, Math.max(0, ms)));
}

function getCloudApiBase(): string {
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

// RFC 8628 device authorization against Phala Cloud. On approval the consume
// step (scope "redpill:api-key") issues a Redpill LLM virtual key — no phak_
// cloud token is created.
async function startDeviceFlow(signal?: AbortSignal): Promise<AciDeviceFlowStart> {
  const codeRes = await fetch(`${getCloudApiBase()}/api/v1/auth/device/code`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    // client_id stays "pi": the consume step (scope redpill:api-key) only
    // recognizes registered CLI client ids; an unknown id passes the code
    // step but 500s at token consume after the user approves.
    body: JSON.stringify({ client_id: "pi", scope: "redpill:api-key" }),
    signal,
  });
  if (!codeRes.ok) {
    throw new Error(`Device authorization request failed: ${await codeRes.text()}`);
  }
  const code = (await codeRes.json()) as DeviceCodeResponse;
  return {
    deviceCode: code.device_code,
    userCode: code.user_code,
    verificationUri: code.verification_uri_complete ?? code.verification_uri,
    intervalSeconds: code.interval,
    expiresInSeconds: code.expires_in,
  };
}

async function pollDeviceFlow(
  start: AciDeviceFlowStart,
  signal?: AbortSignal,
): Promise<AciOAuthCredentials> {
  const deadline = Date.now() + start.expiresInSeconds * 1000;
  let token: DeviceTokenResponse | undefined;
  // RFC 8628 §3.4: poll at the server-provided interval, and back off on
  // slow_down. A loop without this would hammer the token endpoint.
  let intervalMs = Math.max(Number(start.intervalSeconds) || 5, 1) * 1000;
  while (Date.now() < deadline) {
    if (signal?.aborted) throw new Error("Login cancelled");
    const tokenRes = await fetch(`${getCloudApiBase()}/api/v1/auth/device/token`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        device_code: start.deviceCode,
        grant_type: "urn:ietf:params:oauth:grant-type:device_code",
      }),
      signal,
    });
    if (tokenRes.ok) {
      token = (await tokenRes.json()) as DeviceTokenResponse;
      break;
    }
    const raw = await tokenRes.text().catch(() => "");
    const body = (() => {
      try {
        return JSON.parse(raw) as
          | { detail?: { error?: string; error_description?: string } | string }
          | undefined;
      } catch {
        return undefined;
      }
    })();
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
      `HTTP ${tokenRes.status}: ${raw.slice(0, 300)}`;
    throw new Error(`Device authorization failed: ${description}`);
  }
  if (!token) throw new Error("Device authorization expired");

  return {
    // Redpill LLM keys do not expire and cannot be refreshed, so `expires` is
    // set far in the future; a dead key surfaces as a 401 and the user
    // re-runs opencode auth login to mint a new one.
    refresh: "",
    access: token.access_token,
    expires: Date.now() + 100 * 365 * 24 * 60 * 60 * 1000,
  };
}

export default createProvider({
  providerId: "phala",
  label: "Phala Cloud",
  defaultBaseUrl: "https://inference.phala.com/v1",
  apiKeyEnv: "PHALA_LLM_API_KEY",
  envPrefix: "PHALA",
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
    startDeviceFlow,
    pollDeviceFlow,
  },
});
