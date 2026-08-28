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
 *   src/models.ts        — strict /v1/models mapping into Pi models
 *   src/settings-ui.ts   — SettingsList helpers for the settings command
 */

import type {
  ExtensionAPI,
  ExtensionCommandContext,
  ExtensionFactory,
} from "@earendil-works/pi-coding-agent";
import * as piAi from "@earendil-works/pi-ai";
import {
  createProvider as createPiProvider,
  type Model,
  type Provider,
} from "@earendil-works/pi-ai";
import { type SettingItem, SettingsList, truncateToWidth } from "@earendil-works/pi-tui";
import {
  createAciProvider,
  formatAciInspection,
  inspectAciProvider,
  type AccountApiKeyAuth,
  type AciModel,
  type AciInspectionRequest,
  type AciProvider,
  type AciProviderProfile,
} from "@phala/aci-provider";
import os from "node:os";
import { isDeepStrictEqual } from "node:util";

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
import { mapAciModelToPi } from "./src/models.ts";
import { isAciProjectConfigApproved } from "./src/project-trust.ts";
import {
  closeAciProvider,
  ensureAciConnection,
  type AciConnectionState,
} from "./src/connection.ts";
import {
  type AciConfigScope,
  buildSettingsTheme,
  formatScopeDescription,
  modelRegistrationSummary,
  settingsTitle,
} from "./src/settings-ui.ts";

interface AciRuntimeState extends AciConnectionState<AciProvider> {
  profile: ProviderProfile;
  config: AciCloudConfig;
  accountAuth: AccountApiKeyAuth | undefined;
  overrides?: AciCloudConfigPatch;
}

export interface CreatePiAciProviderOptions {
  profile?: Partial<AciProviderProfile>;
  accountAuth?: AccountApiKeyAuth;
  config?: AciCloudConfigPatch;
  footerKey?: string;
}

type OpenAICompletionsApi = ReturnType<
  typeof import("@earendil-works/pi-ai/compat").openAICompletionsApi
>;

function isOpenAICompletionsApi(api: unknown): api is OpenAICompletionsApi {
  return (
    typeof api === "object" &&
    api !== null &&
    "stream" in api &&
    typeof api.stream === "function" &&
    "streamSimple" in api &&
    typeof api.streamSimple === "function"
  );
}

// Pi exposes compat stream factories at the root module for extensions while
// managed installs intentionally omit Pi peer packages.
function getHostOpenAICompletionsApi(): OpenAICompletionsApi {
  if (!("openAICompletionsApi" in piAi)) {
    throw new Error("Pi does not provide the OpenAI Completions API");
  }
  const factory = piAi.openAICompletionsApi;
  if (typeof factory !== "function") {
    throw new Error("Pi provides an invalid OpenAI Completions API factory");
  }
  const api: unknown = factory();
  if (!isOpenAICompletionsApi(api)) {
    throw new Error("Pi provides an invalid OpenAI Completions API");
  }
  return api;
}

function providerFetch(state: AciRuntimeState): typeof globalThis.fetch {
  return async (input, init) => {
    await ensureAciConnection(state, () => createAciProvider(toAciProviderConfig(state.config)));
    if (state.provider) return state.provider.fetch(input, init);
    throw new Error(
      `${state.profile.logPrefix} inference blocked because no verified ACI connection is available: ${state.connectionError ?? "verification has not completed"}`,
    );
  };
}

async function refreshAciModels(
  state: AciRuntimeState,
  signal: AbortSignal,
): Promise<readonly AciModel[]> {
  await ensureAciConnection(state, () => createAciProvider(toAciProviderConfig(state.config)));
  if (!state.provider) {
    throw new Error(
      `${state.profile.logPrefix} model discovery blocked because no verified ACI connection is available: ${state.connectionError ?? "verification has not completed"}`,
    );
  }
  return state.provider.discoverModels({ signal });
}

function toPiModels(
  state: AciRuntimeState,
  models: readonly AciModel[],
): Model<"openai-completions">[] {
  return models.map((model) => ({
    ...mapAciModelToPi(model),
    api: "openai-completions",
    provider: state.profile.providerId,
    baseUrl: state.config.baseUrl,
  }));
}

function nativeAciProvider(state: AciRuntimeState): Provider<"openai-completions"> {
  const streams = getHostOpenAICompletionsApi();
  const fetch = providerFetch(state);
  return createPiProvider({
    id: state.profile.providerId,
    name: state.profile.label,
    baseUrl: state.config.baseUrl,
    auth: { apiKey: createApiKeyAuth(state.profile, state.accountAuth) },
    models: [],
    async fetchModels({ signal }) {
      return toPiModels(state, await refreshAciModels(state, signal));
    },
    api: {
      stream: (model, context, options) => streams.stream(model, context, { ...options, fetch }),
      streamSimple: (model, context, options) =>
        streams.streamSimple(model, context, { ...options, fetch }),
    },
  });
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
  const config = loadAciCloudConfig(
    {
      cwd,
      home: os.homedir(),
      includeProject: projectTrusted,
      profile: state.profile,
    },
    state.overrides,
  );
  if (isDeepStrictEqual(config, state.config)) return;
  state.config = config;
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

async function runInspectionCommand(
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
  request: AciInspectionRequest,
): Promise<void> {
  try {
    await ensureAciConnection(state, () => createAciProvider(toAciProviderConfig(state.config)));
    if (!state.provider) {
      throw new Error(state.connectionError ?? "no verified connection is available");
    }
    const result = await inspectAciProvider(state.provider, request);
    ctx.ui.notify(formatAciInspection(result, { providerLabel: state.profile.label }), "info");
  } catch (error) {
    ctx.ui.notify(
      `ACI inspection failed: ${error instanceof Error ? error.message : String(error)}`,
      "error",
    );
  }
}

/**
 * Create a Pi extension from a shared brand profile and optional account auth.
 * The neutral default profile ("aci") is used when no profile is supplied.
 */
export function createProvider({
  profile: profileOverride,
  accountAuth,
  config: overrides,
  footerKey,
}: CreatePiAciProviderOptions = {}): ExtensionFactory {
  const providerProfile = resolveProfile({ ...profileOverride, footerKey });
  return async (pi: ExtensionAPI) => {
    const cwd = process.cwd();
    const config = loadAciCloudConfig(
      { cwd, home: os.homedir(), includeProject: false, profile: providerProfile },
      overrides,
    );
    const state: AciRuntimeState = {
      profile: providerProfile,
      config,
      accountAuth,
      provider: undefined,
      providerConfigKey: undefined,
      connectionSetup: undefined,
      connectionError: undefined,
      renderConnectionStatus: undefined,
      overrides,
    };
    registerAciProvider(pi, state);

    pi.on("session_start", async (_event, ctx) => {
      state.renderConnectionStatus = () => updateFooter(ctx, state);
      state.renderConnectionStatus?.();
      const projectTrusted = isAciProjectConfigApproved(ctx);
      applyEffectiveConfig(pi, state, ctx.cwd, projectTrusted);
      try {
        await ctx.modelRegistry.refresh({
          providers: [state.profile.providerId],
        });
      } finally {
        state.renderConnectionStatus?.();
      }
    });

    pi.on("session_shutdown", async () => {
      state.renderConnectionStatus = undefined;
      await state.connectionSetup?.promise;
      await closeAciProvider(state);
    });

    const settingsCommand = `${state.profile.providerId}-settings`;
    const attestationCommand = `${state.profile.providerId}-attestation`;
    const receiptsCommand = `${state.profile.providerId}-receipts`;
    const receiptCommand = `${state.profile.providerId}-receipt`;
    const sessionCommand = `${state.profile.providerId}-session`;
    pi.registerCommand(settingsCommand, {
      description: `Configure ${state.profile.label} model discovery`,
      handler: async (_args, ctx) => {
        if (ctx.mode !== "tui") {
          ctx.ui.notify(`${settingsCommand} requires TUI mode`, "error");
          return;
        }
        try {
          await openSettingsMenu(pi, ctx, state);
        } catch (error) {
          ctx.ui.notify(error instanceof Error ? error.message : String(error), "error");
        }
      },
    });

    pi.registerCommand(attestationCommand, {
      description: "Show the cached/current attestation report status",
      handler: async (_args, ctx) => {
        await runInspectionCommand(ctx, state, { action: "attestation" });
      },
    });

    pi.registerCommand(receiptsCommand, {
      description: "List signed ACI receipts retained by this process",
      handler: async (_args, ctx) => {
        await runInspectionCommand(ctx, state, { action: "receipts" });
      },
    });

    pi.registerCommand(receiptCommand, {
      description: "Verify the latest (or a given) signed ACI receipt",
      handler: async (args, ctx) => {
        const id = args?.trim();
        await runInspectionCommand(ctx, state, {
          action: "receipt",
          ...(id ? { id } : {}),
        });
      },
    });

    pi.registerCommand(sessionCommand, {
      description: "Show an attested session document (audit trail)",
      handler: async (args, ctx) => {
        const id = args?.trim();
        if (!id) {
          ctx.ui.notify(`Usage: /${sessionCommand} <session-id>`, "error");
          return;
        }
        await runInspectionCommand(ctx, state, { action: "session", id });
      },
    });
  };
}

export default createProvider();

export const PROVIDER_ID = DEFAULT_PROFILE.providerId;
export { PROVIDER_VERSION };
export { resolveProfile as getProviderProfile } from "./src/profile.ts";
export { loadAciCloudConfig } from "./src/config.ts";
export { discoverAciModels, mapAciModelToPi, mapAciServerModel } from "./src/models.ts";
export { createAciProvider } from "@phala/aci-provider";
