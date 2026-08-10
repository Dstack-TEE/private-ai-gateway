/**
 * Private AI Gateway (ACI) opencode provider plugin.
 *
 * Wires an attested private-ai-gateway into opencode as an OpenAI-compatible
 * provider with attested TLS (SPKI) pinning and per-response receipt
 * verification. This is the vendor-neutral core; branded distributions
 * (opencode-provider-phala-cloud, ...) call createProvider() with their own
 * profile.
 *
 * Usage:
 *   opencode.json: { "plugin": ["@phala/opencode-provider-aci"] }
 *   # Set ACI_LLM_API_KEY (+ ACI_BASE_URL), then select model aci/<model-id>
 *
 * Source layout:
 *   src/profile.ts       — brand identity (provider id, env names, endpoint)
 *   src/constants.ts     — module-level consts + env-driven endpoints
 *   src/config.ts        — layered config (default/plugin options/env/runtime)
 *   src/models.ts        — /v1/models discovery -> opencode models record
 *   src/pinned-fetch.ts  — attested TLS SPKI pinning + request/response capture
 *   src/aci-client.ts    — ACI artifact fetch + verification (reference verifier)
 *   src/verify.ts        — stable re-export surface over aci-client
 *   src/receipt-store.ts — last-response receipt cache + status text
 */

import { type Hooks, type Plugin, type PluginInput, tool } from "@opencode-ai/plugin";

import { type AciCloudConfig, type AciCloudConfigPatch, loadAciCloudConfig } from "./src/config.ts";
import {
  API_KEY_ENV,
  LOG_PREFIX,
  PROVIDER_ID,
  PROVIDER_VERSION,
  applyProviderProfile,
  getEnvApiKey,
  getStoredApiKey,
} from "./src/constants.ts";
import { profile, type ProviderProfile } from "./src/profile.ts";
import { type AciServerModel, discoverAciModels, fallbackModels } from "./src/models.ts";
import { statusText, AciReceiptStore } from "./src/receipt-store.ts";
import { attestedSpkiSha256ForHost } from "./src/verify.ts";
import { TlsPinManager, createAciFetch } from "./src/pinned-fetch.ts";

/** TLS SPKI pin state for the configured base host. */
interface PinningStatus {
  host: string;
  status: "pinned" | "unpinned" | "blocked" | "disabled";
}

interface AciRuntimeState {
  config: AciCloudConfig;
  rawModels: AciServerModel[];
  store: AciReceiptStore;
  pinManager: TlsPinManager;
  pinning?: PinningStatus;
  /** API key captured by the auth loader (opencode auth login credentials). */
  authApiKey?: string;
  /** In-flight pin install (dedupes concurrent first requests). */
  pinPromise?: Promise<boolean>;
}

/** Lowercase hostname of the configured base URL, or undefined when unparseable. */
function hostOfBaseUrl(baseUrl: string): string | undefined {
  try {
    return new URL(baseUrl).hostname.toLowerCase();
  } catch {
    return undefined;
  }
}

function resolveApiKey(state: AciRuntimeState): string {
  // Preference order: the credential captured by the auth loader (this
  // session's login), then the env var, then the credential opencode has on
  // disk from a previous `opencode auth login`.
  return state.authApiKey?.trim() || getEnvApiKey() || getStoredApiKey();
}

/** Debug tracing ({PREFIX}_DEBUG=1): writes exchange/status lines to stderr,
 *  which `opencode run` surfaces. Not for normal operation. */
function debugEnabled(): boolean {
  return process.env[`${profile().envPrefix}_DEBUG`] === "1";
}

function debugLog(...args: unknown[]): void {
  if (debugEnabled()) console.error(LOG_PREFIX, ...args);
}

/**
 * Resolve the attested SPKI for the configured base host from a fresh,
 * validated attestation and install the TLS pin. Default posture is fail
 * CLOSED: with `pinning.enabled` (the default) an unpinnable session blocks
 * inference with a clear error rather than silently downgrading to CA-TLS.
 * Users can opt into the old fail-open behavior via `failOpenOnUnpinned`
 * (runs unpinned with a status warning).
 */
async function ensurePinned(state: AciRuntimeState, host: string): Promise<boolean> {
  if (!state.config.pinning.enabled) {
    state.pinning = { host, status: "disabled" };
    return false;
  }
  if (state.pinManager.getPin(host)) {
    state.pinning = { host, status: "pinned" };
    return true;
  }
  if (!state.pinPromise) {
    state.pinPromise = (async (): Promise<boolean> => {
      const failOpen = state.config.verify.failOpenOnUnpinned === true;
      const unpinned = (): boolean => {
        state.pinning = { host, status: failOpen ? "unpinned" : "blocked" };
        return false;
      };
      const apiKey = resolveApiKey(state);
      if (!apiKey) {
        console.error(
          `${LOG_PREFIX} no API key available; cannot fetch attestation for TLS pinning`,
        );
        return unpinned();
      }
      try {
        const attested = await state.store.getAttestation(apiKey, state.config);
        const spki = attested
          ? attestedSpkiSha256ForHost(state.store.establishedKeyset, host)
          : undefined;
        if (!spki) return unpinned();
        state.pinManager.setPin(host, spki);
        state.pinning = { host, status: "pinned" };
        return true;
      } catch (error) {
        console.error(`${LOG_PREFIX} TLS pin install failed:`, error);
        return unpinned();
      }
    })().finally(() => {
      state.pinPromise = undefined;
    });
  }
  return state.pinPromise;
}

/** Short status suffix describing the TLS pin state. */
function pinSuffix(state: AciRuntimeState): string {
  if (!state.config.pinning.enabled) return "";
  switch (state.pinning?.status) {
    case "pinned":
      return " | tls-pinned";
    case "unpinned":
      return " | UNPINNED";
    case "blocked":
      return " | PIN REQUIRED";
    default:
      return " | pin: pending";
  }
}

function currentStatusText(state: AciRuntimeState): string {
  return statusText(state.store) + pinSuffix(state);
}

/** True when the status is something the user must see (not routine info). */
function isAlertStatus(text: string): boolean {
  return (
    text.includes("mismatch") ||
    text.includes("UNPINNED") ||
    text.includes("PIN REQUIRED") ||
    text.includes("(no receipt)")
  );
}

/** Last status text shown as a toast; identical consecutive statuses are not
 *  re-toasted (a chat turn can trigger several streams, e.g. title gen + main). */
let lastToastedStatus = "";

/** Publish the verification status: always to the opencode log, and to the
 *  TUI as a toast (opencode has no persistent footer surface for plugins, so
 *  the toast is the visible equivalent of pi's footer; alerts stay longer).
 *  The full status is always available via the verification-status tool. */
async function publishStatus(input: PluginInput, state: AciRuntimeState): Promise<void> {
  const text = currentStatusText(state);
  try {
    await input.client.app.log({
      body: { service: PROVIDER_ID, level: "info", message: text },
    });
  } catch {
    // Server log unavailable; never let status publishing break inference.
  }
  if (text === lastToastedStatus) return;
  lastToastedStatus = text;
  const alert = isAlertStatus(text);
  try {
    await input.client.tui.showToast({
      body: {
        title: profile().label,
        message: text,
        variant: alert
          ? text.includes("mismatch") || text.includes("PIN REQUIRED")
            ? "error"
            : "warning"
          : "info",
        duration: alert ? 8000 : 3000,
      },
    });
  } catch {
    // No TUI attached (headless run); the log line above still records it.
  }
}

/** Structured status report for the verification-status tool. */
function statusReport(state: AciRuntimeState): Record<string, unknown> {
  const classification = state.store.classification;
  return {
    provider: PROVIDER_ID,
    version: PROVIDER_VERSION,
    baseUrl: state.config.baseUrl,
    status: currentStatusText(state),
    pinning: state.pinning ?? {
      host: hostOfBaseUrl(state.config.baseUrl) ?? "",
      status: "pending",
    },
    receipt: state.store.snapshot(),
    classification: classification
      ? {
          status: classification.status,
          signatureValid: classification.signatureValid,
          requestHashValid: classification.requestHashValid,
          responseHashValid: classification.responseHashValid,
          hashesChecked: classification.hashesChecked,
          hashesNotCheckedReason: classification.hashesNotCheckedReason,
          provider: classification.provider,
          modelId: classification.modelId,
          sessionId: classification.sessionId,
        }
      : undefined,
    attestation: state.store.binding
      ? {
          workloadKeysetDigest: state.store.binding.workloadKeysetDigest,
          bindingOk: state.store.binding.ok,
        }
      : undefined,
    lastAttestationError: state.store.lastAttestationError,
    modelsRegistered: state.rawModels.length,
  };
}

/**
 * Create the opencode provider plugin for the given brand profile (and
 * optional runtime config patch). The neutral default profile ("aci") is used
 * when no profile is supplied; branded shells pass their own identity.
 */
export function createProvider(
  profileOverride?: Partial<ProviderProfile>,
  overrides?: AciCloudConfigPatch,
): Plugin {
  applyProviderProfile(profileOverride);

  return async (input: PluginInput, pluginOptions?: Record<string, unknown>): Promise<Hooks> => {
    const config = loadAciCloudConfig({ pluginOptions, overrides });
    const state: AciRuntimeState = {
      config,
      rawModels: [],
      store: new AciReceiptStore(),
      pinManager: new TlsPinManager(),
    };
    const baseHost = hostOfBaseUrl(config.baseUrl);

    const aciFetch = createAciFetch({
      manager: state.pinManager,
      isGatewayHost: (host) => baseHost !== undefined && host === baseHost,
      pinningEnabled: () => state.config.pinning.enabled,
      failOpenOnUnpinned: () => state.config.verify.failOpenOnUnpinned,
      ensurePinned: (host) => ensurePinned(state, host),
      onExchange: (exchange) => {
        state.store.recordResponseHeaders(exchange.headers);
        if (exchange.requestBody) state.store.setLastRequestBody(exchange.requestBody);
        if (exchange.responseBytes) state.store.setLastResponseBytes(exchange.responseBytes);
        debugLog("exchange", {
          completion: exchange.completion,
          status: exchange.status,
          receiptId: state.store.snapshot().receiptId,
          responseBytes: exchange.responseBytes?.length,
        });
        if (!state.config.verify.autoFetchReceipt) {
          void publishStatus(input, state);
          return;
        }
        const key = resolveApiKey(state);
        if (!key || !state.store.snapshot().receiptId) {
          void publishStatus(input, state);
          return;
        }
        void (async () => {
          try {
            await state.store.classifyLastResponse(key, state.config);
          } catch (error) {
            console.error(`${LOG_PREFIX} receipt classification failed:`, error);
          }
          debugLog("status", currentStatusText(state));
          await publishStatus(input, state);
        })();
      },
    });

    const oauth = profile().oauth;

    const hooks: Hooks = {
      config: async (cfg) => {
        cfg.provider ??= {};
        const existing = (cfg.provider[PROVIDER_ID] ?? {}) as {
          npm?: string;
          name?: string;
          options?: Record<string, unknown>;
          models?: Record<string, unknown>;
        };

        // Discover the live catalog when a key is available (env only at this
        // point — the auth loader runs later); branded shells fall back to
        // their static catalog. User-declared models always win.
        let models = fallbackModels();
        const key = resolveApiKey(state);
        if (key) {
          const discovered = await discoverAciModels(key, state.config);
          state.rawModels = discovered.raw;
          if (Object.keys(discovered.models).length > 0) models = discovered.models;
        }

        cfg.provider[PROVIDER_ID] = {
          ...existing,
          npm: existing.npm ?? "@ai-sdk/openai-compatible",
          name: existing.name ?? profile().label,
          options: {
            baseURL: state.config.baseUrl,
            // Only pin the env template when the env key is actually set.
            // opencode fills options.apiKey from the auth loader's provider.key
            // ONLY when options.apiKey is undefined — an empty/unresolvable
            // template here would shadow the logged-in credential and send a
            // bad key (observed as gateway 401 "Invalid API key").
            ...(getEnvApiKey() ? { apiKey: `{env:${API_KEY_ENV}}` } : {}),
            ...existing.options,
            // The attested-pinned fetch is not user-overridable: it is the
            // security boundary (pin enforcement + receipt capture).
            fetch: aciFetch,
          },
          models: { ...models, ...existing.models },
        } as (typeof cfg.provider)[string];
      },

      tool: {
        [`${PROVIDER_ID}_verification_status`]: tool({
          description: `Show ${profile().label} verification status: TLS pin state, last receipt classification, attestation binding`,
          args: {},
          async execute() {
            return JSON.stringify(statusReport(state), null, 2);
          },
        }),
        [`${PROVIDER_ID}_settings`]: tool({
          description: `Show or toggle ${profile().label} provider settings. Toggles are runtime-only (persist via plugin options or ${profile().envPrefix}_* env vars).`,
          args: {
            pinning: tool.schema
              .boolean()
              .optional()
              .describe("Require the gateway TLS connection to present the attested SPKI"),
            failOpenOnUnpinned: tool.schema
              .boolean()
              .optional()
              .describe(
                "When true, run unpinned with a warning if no attested pin can be established; when false (default), block inference",
              ),
            autoFetchReceipt: tool.schema
              .boolean()
              .optional()
              .describe("Fetch + verify the receipt after each response"),
            requireAttestationMatch: tool.schema
              .boolean()
              .optional()
              .describe("Require receipts to bind to a validated attestation"),
          },
          async execute(args) {
            if (args.pinning !== undefined) {
              state.config.pinning.enabled = args.pinning;
              if (!args.pinning) {
                const host = state.pinning?.host ?? hostOfBaseUrl(state.config.baseUrl);
                if (host) state.pinManager.clearPin(host);
                state.pinning = { host: host ?? "", status: "disabled" };
              }
            }
            if (args.failOpenOnUnpinned !== undefined) {
              state.config.verify.failOpenOnUnpinned = args.failOpenOnUnpinned;
            }
            if (args.autoFetchReceipt !== undefined) {
              state.config.verify.autoFetchReceipt = args.autoFetchReceipt;
            }
            if (args.requireAttestationMatch !== undefined) {
              state.config.verify.requireAttestationMatch = args.requireAttestationMatch;
            }
            return JSON.stringify(
              {
                provider: PROVIDER_ID,
                effective: {
                  baseUrl: state.config.baseUrl,
                  models: state.config.models,
                  verify: state.config.verify,
                  pinning: state.config.pinning,
                },
                pin: state.pinning,
                note: "Runtime-only changes; persist via plugin options in opencode.json or env vars. isTeeOnly/allowlist changes require a restart (model discovery runs at startup).",
              },
              null,
              2,
            );
          },
        }),
      },
    };

    if (oauth) {
      hooks.auth = {
        provider: PROVIDER_ID,
        loader: async (getAuth) => {
          try {
            const auth = await getAuth();
            if (auth.type === "oauth" && auth.access) {
              state.authApiKey = auth.access;
              return { apiKey: auth.access };
            }
            if (auth.type === "api" && auth.key) {
              state.authApiKey = auth.key;
              return { apiKey: auth.key };
            }
          } catch (error) {
            console.error(`${LOG_PREFIX} auth loader failed:`, error);
          }
          return {};
        },
        methods: [
          {
            type: "oauth",
            label: `Login with ${oauth.name}`,
            authorize: async () => {
              const start = await oauth.startDeviceFlow();
              return {
                url: start.verificationUri,
                instructions: `Open the URL and approve access (code: ${start.userCode}). The CLI polls automatically.`,
                method: "auto" as const,
                callback: async () => {
                  try {
                    const creds = await oauth.pollDeviceFlow(start);
                    return {
                      type: "success" as const,
                      access: creds.access,
                      refresh: creds.refresh,
                      expires: creds.expires,
                    };
                  } catch (error) {
                    console.error(`${LOG_PREFIX} device login failed:`, error);
                    return { type: "failed" as const };
                  }
                },
              };
            },
          },
          {
            type: "api",
            label: `${profile().label} API key`,
            prompts: [
              {
                type: "text",
                key: "key",
                message: `Enter your ${profile().label} API key`,
                validate: (value) => (value.trim() ? undefined : "API key required"),
              },
            ],
            authorize: async (inputs) => {
              const key = inputs?.key?.trim();
              return key ? { type: "success" as const, key } : { type: "failed" as const };
            },
          },
        ],
      };
    }

    return hooks;
  };
}

export { PROVIDER_ID, PROVIDER_VERSION };
export { profile as getProviderProfile } from "./src/profile.ts";
export type {
  AciDeviceFlowStart,
  AciOAuthConfig,
  AciOAuthCredentials,
  ProviderProfile,
} from "./src/profile.ts";
export { loadAciCloudConfig } from "./src/config.ts";
export type { AciCloudConfig, AciCloudConfigPatch } from "./src/config.ts";
export { discoverAciModels, mapAciServerModel, inferReasoning } from "./src/models.ts";
export { TlsPinManager, createAciFetch, computeSpkiSha256Hex } from "./src/pinned-fetch.ts";
export {
  type AttestationReport,
  type ReceiptEnvelope,
  type WorkloadKeyset,
  bindAttestation,
  classifyReceipt,
  fetchAttestation,
  fetchReceipt,
  fetchSession,
  isFullyVerified,
  newNonce,
} from "./src/verify.ts";
