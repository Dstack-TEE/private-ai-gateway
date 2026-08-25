/**
 * Private AI Gateway (ACI) provider extension
 *
 * Wires an attested private-ai-gateway into pi as an OpenAI-compatible provider
 * with an instance-scoped, verified ACI transport. This is the
 * vendor-neutral core; branded distributions (pi-provider-redpill,
 * pi-provider-phala-cloud) call createProvider() with their own profile.
 *
 * Usage:
 *   pi install npm:@phala/pi-provider-aci
 *   # Set ACI_LLM_API_KEY (+ ACI_BASE_URL) then /model aci/<model-id>
 *
 * Source layout:
 *   src/constants.ts     — provider identity + env-driven endpoints
 *   src/config.ts        — layered config (default/home/project/env/runtime)
 *   src/project-trust.ts — project-scope config trust gate
 *   src/models.ts        — /v1/models discovery + thinkingFormat inference
 *   src/audit.ts         — receipt/session fetch and concise audit display
 *   src/settings-ui.ts   — SettingsList helpers for the settings command
 */

import {
  type ExtensionAPI,
  type ExtensionCommandContext,
  type ExtensionFactory,
  readStoredCredential,
} from "@earendil-works/pi-coding-agent";
import { openAICompletionsApi } from "@earendil-works/pi-ai/api/openai-completions.lazy";
import { type SettingItem, SettingsList, truncateToWidth } from "@earendil-works/pi-tui";
import { connectAci, type AciConnection } from "@phala/aci-verifier/node";
import os from "node:os";

import {
  type AciCloudConfig,
  type AciCloudConfigPatch,
  loadHomeAciCloudConfig,
  loadAciCloudConfig,
  loadProjectAciCloudConfig,
  saveHomeAciCloudConfig,
  saveProjectAciCloudConfig,
} from "./src/config.ts";
import { HEADER_RECEIPT_ID, PROVIDER_VERSION } from "./src/constants.ts";
import { DEFAULT_PROFILE, resolveProfile, type ProviderProfile } from "./src/profile.ts";
import {
  type AciServerModel,
  discoverAciModels,
  fallbackModels,
  mapAciServerModel,
} from "./src/models.ts";
import { isAciProjectConfigApproved } from "./src/project-trust.ts";
import { fetchReceipt, fetchSession, summarizeReceipt, summarizeSession } from "./src/audit.ts";
import {
  type AciConfigScope,
  THINKING_FORMAT_VALUES,
  buildSettingsTheme,
  formatScopeDescription,
  modelRegistrationSummary,
  settingsTitle,
} from "./src/settings-ui.ts";

interface AciRuntimeState {
  profile: ProviderProfile;
  cwd: string;
  config: AciCloudConfig;
  projectTrusted: boolean;
  rawModels: AciServerModel[];
  connection: AciConnection | undefined;
  connectionError: string | undefined;
  receiptId: string | undefined;
  overrides?: AciCloudConfigPatch;
}

function resolveApiKey(providerProfile: ProviderProfile): string {
  // Prefer the credential stored by /login (auth.json) to match pi's own
  // auth resolution; fall back to the env var.
  try {
    const stored = readStoredCredential(providerProfile.providerId);
    if (stored?.type === "oauth") {
      // An expired OAuth token must not be sent to the gateway as a live
      // bearer (it fails as a silent 401 that degrades to UNPINNED).
      if (typeof stored.access === "string" && stored.access) {
        const expires = stored.expires;
        if (typeof expires === "number" && Number.isFinite(expires) && expires <= Date.now()) {
          console.error(
            `${providerProfile.logPrefix} stored OAuth credential is expired; run /login ${providerProfile.providerId} again`,
          );
        } else {
          return stored.access;
        }
      }
    } else if (stored?.type === "api_key" && typeof stored.key === "string" && stored.key) {
      return stored.key;
    }
  } catch {
    // auth.json unreadable; fall through to env.
  }
  for (const name of [providerProfile.apiKeyEnv, ...(providerProfile.apiKeyAliases ?? [])]) {
    const value = process.env[name]?.trim();
    if (value) return value;
  }
  return "";
}

function modelsFromState(state: AciRuntimeState): ReturnType<typeof fallbackModels> {
  const mapped = state.rawModels
    .map((m) => mapAciServerModel(m, state.config))
    .filter((m): m is ReturnType<typeof fallbackModels>[number] => m !== null);
  return mapped.length > 0 ? mapped : fallbackModels(state.profile);
}

/**
 * Establish an instance-scoped ACI connection for the configured gateway.
 * Default posture is fail closed: when verification or channel binding fails,
 * the provider stream receives a rejecting fetch implementation.
 */
async function installAciConnection(state: AciRuntimeState): Promise<void> {
  await closeAciConnection(state);

  const apiKey = resolveApiKey(state.profile);
  if (!apiKey) {
    state.connectionError = "API key is not configured";
    return;
  }

  try {
    const connection = await connectAci({
      baseURL: state.config.baseUrl,
      apiKey,
      policy:
        state.config.trust.acceptedComposeHashes === undefined
          ? {}
          : { acceptedComposeHashes: state.config.trust.acceptedComposeHashes },
    });
    state.connection = connection;
    state.connectionError = undefined;
  } catch (error) {
    state.connectionError = error instanceof Error ? error.message : String(error);
    console.error(`${state.profile.logPrefix} ACI connection failed:`, error);
  }
}

async function closeAciConnection(state: AciRuntimeState): Promise<void> {
  const connection = state.connection;
  state.connection = undefined;
  if (!connection) return;
  try {
    await connection.close();
  } catch (error) {
    console.error(`${state.profile.logPrefix} ACI connection close failed:`, error);
  }
}

const openAICompletions = openAICompletionsApi();

function providerFetch(state: AciRuntimeState): typeof globalThis.fetch {
  if (state.connection) return state.connection.fetch;
  return () =>
    Promise.reject(
      new Error(
        `${state.profile.logPrefix} inference blocked because no verified ACI connection is available: ${state.connectionError ?? "verification has not completed"}`,
      ),
    );
}

function registerAciProvider(pi: ExtensionAPI, state: AciRuntimeState): void {
  const config = state.config;
  const providerProfile = state.profile;
  const oauth = providerProfile.oauth;
  pi.registerProvider(providerProfile.providerId, {
    baseUrl: config.baseUrl,
    apiKey: `$${providerProfile.apiKeyEnv}`,
    api: "openai-completions",
    authHeader: true,
    models: modelsFromState(state),
    streamSimple: (model, context, options) =>
      openAICompletions.streamSimple(model, context, {
        ...options,
        fetch: providerFetch(state),
      }),
    ...(oauth ? { oauth } : {}),
  });
}

function reloadEffectiveConfig(
  state: AciRuntimeState,
  cwd: string,
  projectTrusted: boolean,
): AciCloudConfig {
  const config = loadAciCloudConfig(
    {
      cwd,
      home: os.homedir(),
      includeProject: projectTrusted,
      profile: state.profile,
    },
    state.overrides,
  );
  state.cwd = cwd;
  state.config = config;
  state.projectTrusted = projectTrusted;
  return config;
}

function applyEffectiveConfig(
  pi: ExtensionAPI,
  state: AciRuntimeState,
  cwd: string,
  projectTrusted: boolean,
): void {
  reloadEffectiveConfig(state, cwd, projectTrusted);
  registerAciProvider(pi, state);
}

function updateFooter(
  ctx: { ui: { setStatus: (key: string, text: string | undefined) => void } },
  state: AciRuntimeState,
): void {
  try {
    ctx.ui.setStatus(state.profile.footerKey, connectionSuffix(state));
  } catch {
    // The session may have been replaced/reloaded between an async update and
    // this render; the captured ctx is stale. Nothing to render to.
  }
}

/** Short footer suffix describing the verified transport state. */
function connectionSuffix(state: AciRuntimeState): string {
  if (state.connection) return " | aci-verified";
  if (state.connectionError) return " | ACI BLOCKED";
  return " | aci: pending";
}

async function openSettingsMenu(
  pi: ExtensionAPI,
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
): Promise<void> {
  const projectTrusted = isAciProjectConfigApproved(ctx);
  const homeDraft = loadHomeAciCloudConfig(os.homedir(), state.profile);
  const drafts: Record<AciConfigScope, AciCloudConfig> = {
    project: projectTrusted ? loadProjectAciCloudConfig(ctx.cwd, state.profile) : homeDraft,
    home: homeDraft,
  };
  let scope: AciConfigScope = projectTrusted ? "project" : "home";
  let dirty = false;

  await ctx.ui.custom<void>((tui, theme, _keybindings, done) => {
    const settingsTheme = buildSettingsTheme(theme);
    let list: SettingsList;

    const refreshValues = () => {
      list.updateValue("scope", scope);
      list.updateValue("isTeeOnly", drafts[scope].models.isTeeOnly ? "true" : "false");
      list.updateValue("thinkingFormat", drafts[scope].models.thinkingFormat);
    };

    const save = () => {
      if (scope === "project" && !projectTrusted) {
        ctx.ui.notify("Project config cannot be saved until the project is trusted.", "warning");
        return;
      }
      try {
        if (scope === "project") {
          saveProjectAciCloudConfig(ctx.cwd, drafts[scope], state.profile.providerId);
        } else {
          saveHomeAciCloudConfig(os.homedir(), drafts[scope], state.profile.providerId);
        }
        applyEffectiveConfig(pi, state, ctx.cwd, scope === "project" ? true : projectTrusted);
        dirty = true;
      } catch (error: unknown) {
        ctx.ui.notify((error as Error).message, "error");
      }
    };

    const onChange = (id: string, newValue: string) => {
      if (id === "scope") {
        scope = newValue as AciConfigScope;
        refreshValues();
        return;
      }
      if (id === "isTeeOnly") {
        drafts[scope].models.isTeeOnly = newValue === "true";
        list.updateValue(id, newValue);
        save();
        return;
      }
      if (id === "thinkingFormat") {
        drafts[scope].models.thinkingFormat =
          newValue as AciCloudConfig["models"]["thinkingFormat"];
        list.updateValue(id, newValue);
        save();
        return;
      }
    };

    const scopeItem: SettingItem = {
      id: "scope",
      label: "Config scope",
      description: projectTrusted
        ? formatScopeDescription(scope, ctx.cwd, os.homedir(), state.profile.providerId)
        : "Project config disabled until the project is trusted; editing home config only",
      currentValue: scope,
      values: projectTrusted ? ["project", "home"] : ["home"],
    };

    const items: SettingItem[] = [
      scopeItem,
      {
        id: "isTeeOnly",
        label: "TEE-only models",
        description: "Only register models served confidentially (is_tee === true)",
        currentValue: drafts[scope].models.isTeeOnly ? "true" : "false",
        values: ["true", "false"],
      },
      {
        id: "thinkingFormat",
        label: "Thinking format",
        description: "How pi thinking levels map to provider parameters",
        currentValue: drafts[scope].models.thinkingFormat,
        values: [...THINKING_FORMAT_VALUES],
      },
    ];

    list = new SettingsList(items, items.length, settingsTheme, onChange, () => done(), {
      enableSearch: true,
    });

    return {
      items,
      onChange,
      render(width: number) {
        return [
          truncateToWidth(
            theme.fg("accent", theme.bold(settingsTitle(state.profile.label))),
            width,
          ),
          "",
          truncateToWidth(modelRegistrationSummary(drafts[scope]), width),
          "",
          ...list.render(width),
        ];
      },
      handleInput(data: string) {
        list.handleInput?.(data);
        tui.requestRender();
      },
      invalidate() {
        list.invalidate();
      },
    };
  });

  if (dirty) await ctx.reload();
}

/** On-request audit: fetch and show the latest (or a given) receipt. Raw
 *  document display — no signature verification (prevention is pinning). */
async function runReceiptCommand(
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
  args: string,
): Promise<void> {
  const apiKey = resolveApiKey(state.profile);
  if (!apiKey) {
    ctx.ui.notify(`${state.profile.apiKeyEnv} not set`, "error");
    return;
  }
  const receiptId = args.trim() || state.receiptId;
  if (!receiptId) {
    ctx.ui.notify(
      "No receipt id given and no x-receipt-id seen yet; send a message first or pass an id",
      "error",
    );
    return;
  }
  const receipt = await fetchReceipt(apiKey, receiptId, {
    baseUrl: state.config.baseUrl,
    fetch: providerFetch(state),
    logPrefix: state.profile.logPrefix,
  });
  if (!receipt) {
    ctx.ui.notify(`Receipt ${receiptId} not found or fetch failed`, "error");
    return;
  }
  ctx.ui.notify(summarizeReceipt(receipt).join("\n"), "info");
}

/** On-request audit: fetch and show an attested session document (raw). */
async function runSessionCommand(
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
  args: string,
): Promise<void> {
  const apiKey = resolveApiKey(state.profile);
  if (!apiKey) {
    ctx.ui.notify(`${state.profile.apiKeyEnv} not set`, "error");
    return;
  }
  const sessionId = args.trim();
  if (!sessionId) {
    ctx.ui.notify("Usage: /aci-session <session-id>", "error");
    return;
  }
  const session = await fetchSession(apiKey, sessionId, {
    baseUrl: state.config.baseUrl,
    fetch: providerFetch(state),
    logPrefix: state.profile.logPrefix,
  });
  if (!session) {
    ctx.ui.notify(`Session ${sessionId} not found or fetch failed`, "error");
    return;
  }
  ctx.ui.notify(summarizeSession(session).join("\n"), "info");
}

async function runAttestationCommand(
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
): Promise<void> {
  const apiKey = resolveApiKey(state.profile);
  if (!apiKey) {
    ctx.ui.notify(`${state.profile.apiKeyEnv} not set`, "error");
    return;
  }
  if (!state.connection) {
    ctx.ui.notify(
      `Attestation validation failed: ${state.connectionError ?? "no verified connection"}`,
      "error",
    );
    return;
  }
  const identity = state.connection.identity;
  const report = identity.report;
  const keyset = identity.keyset;
  const e2eeKeys = keyset.e2ee_public_keys;
  const receiptKeys = keyset.receipt_signing_keys;
  const notAfter = keyset.not_after;
  const releasePolicy = state.config.trust.acceptedComposeHashes?.length
    ? "accepted"
    : "measurement verified, not pinned";
  const keySummary = (keys: Array<{ key_id?: unknown; algo?: unknown }>) =>
    keys.length === 0
      ? "none"
      : keys.map((k) => `${String(k.key_id)} (${String(k.algo)})`).join(", ");
  const lines = [
    `${state.profile.label} attestation`,
    `API version: ${String(report.api_version)}`,
    `Keyset digest: ${identity.workloadKeysetDigest}`,
    `Compose hash: ${identity.composeHash}`,
    `Release policy: ${releasePolicy}`,
    `Report binding: verified`,
    `TLS SPKI pins: ${identity.tlsSpkiPins.join(", ")}`,
    `Keyset not_after: ${notAfter !== undefined ? new Date(notAfter * 1000).toISOString() : "unknown"}`,
    `Encryption keys (${e2eeKeys.length}): ${keySummary(e2eeKeys)}`,
    `Receipt signing keys (${receiptKeys.length}): ${keySummary(receiptKeys)}`,
  ];
  ctx.ui.notify(lines.join("\n"), "info");
}

/**
 * Create the provider extension for the given brand profile (and optional
 * runtime config patch). The neutral default profile ("aci") is used when no
 * profile is supplied; branded shells pass their own identity.
 */
export function createProvider(
  profileOverride?: Partial<ProviderProfile>,
  overrides?: AciCloudConfigPatch,
): ExtensionFactory {
  const providerProfile = resolveProfile(profileOverride);
  return async (pi: ExtensionAPI) => {
    const cwd = process.cwd();
    const config = loadAciCloudConfig(
      { cwd, home: os.homedir(), includeProject: false, profile: providerProfile },
      overrides,
    );
    const state: AciRuntimeState = {
      profile: providerProfile,
      cwd,
      config,
      projectTrusted: false,
      rawModels: [],
      connection: undefined,
      connectionError: undefined,
      receiptId: undefined,
      overrides,
    };
    registerAciProvider(pi, state);

    pi.on("session_start", async (_event, ctx) => {
      const projectTrusted = isAciProjectConfigApproved(ctx);
      applyEffectiveConfig(pi, state, ctx.cwd, projectTrusted);
      await installAciConnection(state);
      const sessionApiKey = resolveApiKey(state.profile);
      if (sessionApiKey) {
        const discovered = await discoverAciModels(sessionApiKey, state.config, {
          baseUrl: state.config.baseUrl,
          fetch: providerFetch(state),
          logPrefix: state.profile.logPrefix,
        });
        state.rawModels = discovered.raw;
        registerAciProvider(pi, state);
      }
      updateFooter(ctx, state);
    });

    pi.on("session_shutdown", async () => {
      await closeAciConnection(state);
    });

    // Cheap header capture ONLY: remember the latest x-receipt-id so the
    // on-request /aci-receipt audit command knows what to fetch. No receipt is
    // downloaded or verified here (prevention is pinning; audit is opt-in).
    pi.on("after_provider_response", (event, ctx) => {
      if (ctx.model?.provider !== state.profile.providerId) return;
      const lower = Object.fromEntries(
        Object.entries(event.headers ?? {}).map(([k, v]) => [k.toLowerCase(), v]),
      );
      state.receiptId = lower[HEADER_RECEIPT_ID] ?? lower["x-receipt-id"];
    });

    const settingsCommand = `${state.profile.providerId}-settings`;
    pi.registerCommand(settingsCommand, {
      description: `Configure ${state.profile.label} models and thinking format`,
      handler: async (_args, ctx) => {
        if (ctx.mode !== "tui") {
          ctx.ui.notify(`${settingsCommand} requires TUI mode`, "error");
          return;
        }
        await openSettingsMenu(pi, ctx, state);
      },
    });

    pi.registerCommand("attestation", {
      description: "Show the cached/current attestation report status",
      handler: async (_args, ctx) => {
        await runAttestationCommand(ctx, state);
      },
    });

    pi.registerCommand("aci-receipt", {
      description: "Show the latest (or a given) receipt as an audit trail (no verification)",
      handler: async (args, ctx) => {
        await runReceiptCommand(ctx, state, args ?? "");
      },
    });

    pi.registerCommand("aci-session", {
      description: "Show an attested session document (audit trail)",
      handler: async (args, ctx) => {
        await runSessionCommand(ctx, state, args ?? "");
      },
    });
  };
}

export default createProvider();

export const PROVIDER_ID = DEFAULT_PROFILE.providerId;
export { PROVIDER_VERSION };
export { resolveProfile as getProviderProfile } from "./src/profile.ts";
export { loadAciCloudConfig } from "./src/config.ts";
export { discoverAciModels, mapAciServerModel, inferThinkingFormat } from "./src/models.ts";
export { connectAci } from "@phala/aci-verifier/node";
