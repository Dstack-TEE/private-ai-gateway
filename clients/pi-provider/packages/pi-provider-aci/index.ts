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
 *   # Set ACI_API_KEY (+ ACI_BASE_URL) then /model aci/<model-id>
 *
 * Source layout:
 *   src/config.ts        — layered config (default/home/project/env/runtime)
 *   src/project-trust.ts — project-scope config trust gate
 *   src/models.ts        — /v1/models discovery + thinkingFormat inference
 *   src/audit.ts         — concise receipt/session audit display
 *   src/settings-ui.ts   — SettingsList helpers for the settings command
 */

import type {
  ExtensionAPI,
  ExtensionCommandContext,
  ExtensionFactory,
} from "@earendil-works/pi-coding-agent";
import type { Model, Provider } from "@earendil-works/pi-ai";
import { openAICompletionsApi } from "@earendil-works/pi-ai/api/openai-completions.lazy";
import { type SettingItem, SettingsList, truncateToWidth } from "@earendil-works/pi-tui";
import { createAciProvider, type AciProvider } from "@phala/aci-provider";
import os from "node:os";

import {
  type AciCloudConfig,
  type AciCloudConfigPatch,
  loadHomeAciCloudConfig,
  loadAciCloudConfig,
  loadProjectAciCloudConfig,
  saveHomeAciCloudConfig,
  saveProjectAciCloudConfig,
  toAciProviderConfig,
} from "./src/config.ts";
import { createApiKeyAuth } from "./src/auth.ts";
import { PROVIDER_VERSION } from "./src/constants.ts";
import { DEFAULT_PROFILE, resolveProfile, type ProviderProfile } from "./src/profile.ts";
import { type AciServerModel, discoverAciModels, mapAciServerModel } from "./src/models.ts";
import { isAciProjectConfigApproved } from "./src/project-trust.ts";
import { summarizeReceipt, summarizeSession } from "./src/audit.ts";
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
  config: AciCloudConfig;
  rawModels: AciServerModel[];
  provider: AciProvider | undefined;
  providerConfigKey: string | undefined;
  connectionSetup: AciConnectionSetup | undefined;
  connectionError: string | undefined;
  overrides?: AciCloudConfigPatch;
}

interface AciConnectionSetup {
  configKey: string;
  promise: Promise<void>;
}

function modelsFromState(state: AciRuntimeState) {
  return state.rawModels
    .map((m) => mapAciServerModel(m, state.config))
    .filter((m): m is NonNullable<typeof m> => m !== null);
}

/**
 * Establish an instance-scoped ACI connection for the configured gateway.
 * Default posture is fail closed: when verification or channel binding fails,
 * the provider stream receives a rejecting fetch implementation.
 */
function connectionConfig(config: AciCloudConfig): string {
  return JSON.stringify({
    baseUrl: config.baseUrl,
    acceptedComposeHashes: config.trust.acceptedComposeHashes,
    acceptedSessionIds: config.trust.acceptedSessionIds,
  });
}

async function ensureAciConnection(state: AciRuntimeState): Promise<void> {
  const config = state.config;
  const configKey = connectionConfig(config);
  if (state.provider && state.providerConfigKey === configKey) {
    return;
  }

  const activeSetup = state.connectionSetup;
  if (activeSetup) {
    await activeSetup.promise;
    if (activeSetup.configKey === configKey) return;
    return ensureAciConnection(state);
  }

  const setup = (async () => {
    await closeAciProvider(state);
    try {
      const provider = createAciProvider(toAciProviderConfig(config));
      await provider.connect();
      state.provider = provider;
      state.providerConfigKey = configKey;
      state.connectionError = undefined;
    } catch (error) {
      state.connectionError = error instanceof Error ? error.message : String(error);
      console.error(`${state.profile.logPrefix} ACI connection failed:`, error);
    }
  })();
  const pending = { configKey, promise: setup };
  state.connectionSetup = pending;
  try {
    await setup;
  } finally {
    if (state.connectionSetup === pending) state.connectionSetup = undefined;
  }
}

async function closeAciProvider(state: AciRuntimeState): Promise<void> {
  const provider = state.provider;
  state.provider = undefined;
  state.providerConfigKey = undefined;
  if (!provider) return;
  try {
    await provider.close();
  } catch (error) {
    console.error(`${state.profile.logPrefix} ACI connection close failed:`, error);
  }
}

function providerFetch(state: AciRuntimeState): typeof globalThis.fetch {
  return async (input, init) => {
    await ensureAciConnection(state);
    if (state.provider) return state.provider.fetch(input, init);
    throw new Error(
      `${state.profile.logPrefix} inference blocked because no verified ACI connection is available: ${state.connectionError ?? "verification has not completed"}`,
    );
  };
}

async function refreshAciModels(
  state: AciRuntimeState,
  signal: AbortSignal,
): Promise<AciServerModel[]> {
  await ensureAciConnection(state);
  if (!state.provider) {
    throw new Error(
      `${state.profile.logPrefix} model discovery blocked because no verified ACI connection is available: ${state.connectionError ?? "verification has not completed"}`,
    );
  }
  const discovered = await discoverAciModels(state.config, {
    baseUrl: state.config.baseUrl,
    fetch: providerFetch(state),
    signal,
  });
  return discovered.raw;
}

function piModelsFromState(state: AciRuntimeState): Model<"openai-completions">[] {
  return modelsFromState(state).map((model) => ({
    ...model,
    api: "openai-completions",
    provider: state.profile.providerId,
    baseUrl: state.config.baseUrl,
  }));
}

function nativeAciProvider(state: AciRuntimeState): Provider<"openai-completions"> {
  const streams = openAICompletionsApi();
  const fetch = providerFetch(state);
  return {
    id: state.profile.providerId,
    name: state.profile.label,
    baseUrl: state.config.baseUrl,
    auth: { apiKey: createApiKeyAuth(state.profile) },
    getModels: () => piModelsFromState(state),
    async refreshModels(context) {
      if (!context.allowNetwork) return;
      const rawModels = await refreshAciModels(state, context.signal);
      await context.publish({
        update: () => {
          state.rawModels = rawModels;
        },
      });
    },
    stream: (model, context, options) => streams.stream(model, context, { ...options, fetch }),
    streamSimple: (model, context, options) =>
      streams.streamSimple(model, context, { ...options, fetch }),
  };
}

function registerAciProvider(pi: ExtensionAPI, state: AciRuntimeState): void {
  pi.registerProvider(nativeAciProvider(state));
}

function applyEffectiveConfig(
  pi: ExtensionAPI,
  state: AciRuntimeState,
  cwd: string,
  projectTrusted: boolean,
): void {
  state.config = loadAciCloudConfig(
    {
      cwd,
      home: os.homedir(),
      includeProject: projectTrusted,
      profile: state.profile,
    },
    state.overrides,
  );
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
  if (state.provider?.status().phase === "verified") return " | aci-verified";
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
        ctx.ui.notify(error instanceof Error ? error.message : String(error), "error");
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

/** Verify and show the latest recorded receipt, or a given receipt id. */
async function runReceiptCommand(
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
  args: string,
): Promise<void> {
  if (!state.provider) {
    ctx.ui.notify(
      `ACI connection unavailable: ${state.connectionError ?? "not verified"}`,
      "error",
    );
    return;
  }
  const receiptId = args.trim() || state.provider.receipts()[0]?.receiptId;
  if (!receiptId) {
    ctx.ui.notify(
      "No receipt id given and no x-receipt-id seen yet; send a message first or pass an id",
      "error",
    );
    return;
  }
  try {
    const audit = await state.provider.verifyReceipt(receiptId);
    const checks = audit.transcript.lines.map((line) => {
      const detail = line.detail ? `: ${line.detail}` : "";
      return `${line.status.toUpperCase()} ${line.id}${detail}`;
    });
    ctx.ui.notify(
      [
        ...summarizeReceipt(audit.receipt),
        `Verdict: ${audit.transcript.verdict.line}`,
        ...checks,
      ].join("\n"),
      audit.transcript.verdict.verified ? "info" : "error",
    );
  } catch (error) {
    ctx.ui.notify(
      `Receipt ${receiptId} verification failed: ${error instanceof Error ? error.message : String(error)}`,
      "error",
    );
  }
}

/** Fetch, verify, and summarize an attested session document on request. */
async function runSessionCommand(
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
  args: string,
): Promise<void> {
  await ensureAciConnection(state);
  if (!state.provider) {
    ctx.ui.notify(
      `ACI connection unavailable: ${state.connectionError ?? "not verified"}`,
      "error",
    );
    return;
  }
  const sessionId = args.trim();
  if (!sessionId) {
    ctx.ui.notify(`Usage: /${state.profile.providerId}-session <session-id>`, "error");
    return;
  }
  try {
    const audit = await state.provider.verifySession(sessionId);
    const checks = audit.checks.map(
      (check) => `${check.ok ? "PASS" : "FAIL"} ${check.name}: ${check.detail}`,
    );
    ctx.ui.notify(
      [...summarizeSession(audit.session, sessionId), ...checks].join("\n"),
      audit.verified ? "info" : "error",
    );
  } catch (error) {
    ctx.ui.notify(
      `Session ${sessionId} verification failed: ${error instanceof Error ? error.message : String(error)}`,
      "error",
    );
  }
}

async function runAttestationCommand(
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
): Promise<void> {
  await ensureAciConnection(state);
  if (!state.provider) {
    ctx.ui.notify(
      `Attestation validation failed: ${state.connectionError ?? "no verified connection"}`,
      "error",
    );
    return;
  }
  const identity = state.provider.status().identity;
  if (!identity) {
    ctx.ui.notify("Attestation validation failed: verified identity unavailable", "error");
    return;
  }
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
      config,
      rawModels: [],
      provider: undefined,
      providerConfigKey: undefined,
      connectionSetup: undefined,
      connectionError: undefined,
      overrides,
    };
    registerAciProvider(pi, state);

    pi.on("session_start", async (_event, ctx) => {
      const projectTrusted = isAciProjectConfigApproved(ctx);
      applyEffectiveConfig(pi, state, ctx.cwd, projectTrusted);
      await ctx.modelRegistry.refresh({
        allowNetwork: true,
        providers: [state.profile.providerId],
      });
      updateFooter(ctx, state);
    });

    pi.on("session_shutdown", async () => {
      await state.connectionSetup?.promise;
      await closeAciProvider(state);
    });

    const settingsCommand = `${state.profile.providerId}-settings`;
    const attestationCommand = `${state.profile.providerId}-attestation`;
    const receiptCommand = `${state.profile.providerId}-receipt`;
    const sessionCommand = `${state.profile.providerId}-session`;
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

    pi.registerCommand(attestationCommand, {
      description: "Show the cached/current attestation report status",
      handler: async (_args, ctx) => {
        await runAttestationCommand(ctx, state);
      },
    });

    pi.registerCommand(receiptCommand, {
      description: "Verify the latest (or a given) signed ACI receipt",
      handler: async (args, ctx) => {
        await runReceiptCommand(ctx, state, args ?? "");
      },
    });

    pi.registerCommand(sessionCommand, {
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
export { createAciProvider } from "@phala/aci-provider";
