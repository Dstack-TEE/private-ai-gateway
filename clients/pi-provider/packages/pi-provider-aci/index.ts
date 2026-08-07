/**
 * Private AI Gateway (ACI) provider extension
 *
 * Wires an attested private-ai-gateway into pi as an OpenAI-compatible provider
 * with per-response verifiability and attested TLS (SPKI) pinning. This is the
 * vendor-neutral core; branded distributions (pi-provider-redpill,
 * pi-provider-phala-cloud) call createProvider() with their own profile.
 *
 * Usage:
 *   pi install npm:@phala/pi-provider-aci
 *   # Set ACI_LLM_API_KEY (+ ACI_BASE_URL) then /model aci/<model-id>
 *
 * Source layout:
 *   src/constants.ts     — module-level consts + env-driven endpoints
 *   src/config.ts        — layered config (default/home/project/env/runtime)
 *   src/project-trust.ts — project-scope config trust gate
 *   src/canonical.ts     — JCS (RFC 8785 subset) for receipt/attestation digests
 *   src/crypto.ts        — ed25519 receipt signatures + hash helpers
 *   src/tls-pinning.ts   — attested TLS SPKI pinning via a narrow fetch wrapper
 *   src/models.ts        — /v1/models discovery + thinkingFormat inference
 *   src/verify.ts        — receipt/attestation/session fetch + full verification
 *   src/receipt-store.ts — last-response receipt cache + footer status source
 *   src/settings-ui.ts   — SettingsList helpers for the settings command
 */

import {
  type ExtensionAPI,
  type ExtensionCommandContext,
  type ExtensionFactory,
  readStoredCredential,
} from "@earendil-works/pi-coding-agent";
import { type SettingItem, SettingsList, truncateToWidth } from "@earendil-works/pi-tui";
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
import {
  API_KEY_ENV,
  FOOTER_STATUS_KEY,
  LOG_PREFIX,
  PROVIDER_ID,
  PROVIDER_VERSION,
  applyProviderProfile,
} from "./src/constants.ts";
import { profile, type ProviderProfile } from "./src/profile.ts";
import {
  type AciServerModel,
  discoverAciModels,
  fallbackModels,
  mapAciServerModel,
} from "./src/models.ts";
import { isAciProjectConfigApproved } from "./src/project-trust.ts";
import { footerText, AciReceiptStore } from "./src/receipt-store.ts";
import { attestedSpkiSha256ForHost } from "./src/verify.ts";
import type { WorkloadKeyset } from "./src/verify.ts";
import {
  clearPin,
  installFetchPinning,
  requirePinForHost,
  setPin,
  unrequirePinForHost,
} from "./src/tls-pinning.ts";
import {
  type AciConfigScope,
  THINKING_FORMAT_VALUES,
  buildSettingsTheme,
  formatScopeDescription,
  modelRegistrationSummary,
  settingsTitle,
  verifySummary,
} from "./src/settings-ui.ts";

/** TLS SPKI pin state for the configured base host, shown in the footer. */
interface PinningStatus {
  host: string;
  status: "pinned" | "unpinned" | "blocked" | "disabled";
}

interface AciRuntimeState {
  cwd: string;
  config: AciCloudConfig;
  projectTrusted: boolean;
  rawModels: AciServerModel[];
  store: AciReceiptStore;
  /** TLS SPKI pin status for the configured base host (attested, per session). */
  pinning?: PinningStatus;
  overrides?: AciCloudConfigPatch;
}

/** Lowercase hostname of the configured base URL, or undefined when unparseable. */
function hostOfBaseUrl(baseUrl: string): string | undefined {
  try {
    return new URL(baseUrl).hostname.toLowerCase();
  } catch {
    return undefined;
  }
}

function resolveApiKey(): string {
  // Prefer the credential stored by /login (auth.json) to match pi's own
  // auth resolution; fall back to the env var.
  try {
    const stored = readStoredCredential(PROVIDER_ID);
    if (stored?.type === "oauth") {
      // An expired OAuth token must not be sent to the gateway as a live
      // bearer (it fails as a silent 401 that degrades to UNPINNED/verified*).
      if (typeof stored.access === "string" && stored.access) {
        const expires = stored.expires;
        if (typeof expires === "number" && Number.isFinite(expires) && expires <= Date.now()) {
          console.error(`${LOG_PREFIX} stored OAuth credential is expired; run /login ${PROVIDER_ID} again`);
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
  return process.env[API_KEY_ENV]?.trim() || "";
}

/** Human-facing label of the active brand profile. */
function getLabel(): string {
  return profile().label;
}

function modelsFromState(state: AciRuntimeState): ReturnType<typeof fallbackModels> {
  const mapped = state.rawModels
    .map((m) => mapAciServerModel(m, state.config))
    .filter((m): m is ReturnType<typeof fallbackModels>[number] => m !== null);
  return mapped.length > 0 ? mapped : fallbackModels();
}

/**
 * Resolve the attested SPKI for the configured base host from a fresh,
 * validated attestation and install the TLS pin. Default posture is fail
 * CLOSED: with `pinning.enabled` (the default) an unpinnable session blocks
 * inference with a clear error rather than silently downgrading to CA-TLS.
 * Users can opt into the old fail-open behavior via
 * `verify.failOpenOnUnpinned` (runs unpinned with a footer warning).
 */
async function installAttestedTlsPin(state: AciRuntimeState): Promise<void> {
  const config = state.config;
  const host = hostOfBaseUrl(config.baseUrl);
  const normalized = host ?? "";

  // Pinning disabled: no pin, no fail-closed gate (explicit user choice).
  if (!config.pinning.enabled) {
    if (host) {
      clearPin(host);
      unrequirePinForHost(host);
    }
    state.pinning = { host: normalized, status: "disabled" };
    return;
  }
  if (!host) {
    state.pinning = { host: config.baseUrl, status: "unpinned" };
    return;
  }

  // Drop a stale pin from an earlier session for a different host; ensure this
  // host is marked required so traffic fails closed until a pin is installed.
  if (state.pinning?.host && state.pinning.host !== normalized) {
    clearPin(state.pinning.host);
    unrequirePinForHost(state.pinning.host);
  }
  requirePinForHost(host);

  const apiKey = resolveApiKey();
  if (!apiKey) {
    clearPin(host);
    state.pinning = { host, status: "unpinned" };
    return;
  }

  const failOpen = config.verify.failOpenOnUnpinned === true;
  const unpinned = (): void => {
    clearPin(host);
    state.pinning = { host, status: failOpen ? "unpinned" : "blocked" };
  };

  try {
    const attested = await state.store.getAttestation(apiKey, config);
    if (!attested) {
      unpinned();
      return;
    }
    const spki = attestedSpkiSha256ForHost(state.store.establishedKeyset, host);
    if (!spki) {
      unpinned();
      return;
    }
    installFetchPinning();
    setPin(host, spki);
    requirePinForHost(host);
    state.pinning = { host, status: "pinned" };
  } catch (error) {
    console.error(`${LOG_PREFIX} TLS pin install failed:`, error);
    unpinned();
  }
}

function registerAciProvider(pi: ExtensionAPI, state: AciRuntimeState): void {
  const config = state.config;
  const oauth = profile().oauth;
  pi.registerProvider(PROVIDER_ID, {
    baseUrl: config.baseUrl,
    apiKey: `$${API_KEY_ENV}`,
    api: "openai-completions",
    authHeader: true,
    models: modelsFromState(state),
    ...(oauth ? { oauth } : {}),
  });
}

function reloadEffectiveConfig(
  state: AciRuntimeState,
  cwd: string,
  projectTrusted: boolean,
): AciCloudConfig {
  const config = loadAciCloudConfig(
    { cwd, home: os.homedir(), includeProject: projectTrusted },
    state.overrides,
  );
  state.cwd = cwd;
  state.config = config;
  state.projectTrusted = projectTrusted;
  state.pinning = undefined;
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
    ctx.ui.setStatus(FOOTER_STATUS_KEY, footerText(state.store) + pinSuffix(state));
  } catch {
    // The session may have been replaced/reloaded between the async receipt
    // fetch and this update; the captured ctx is stale. Nothing to render to.
  }
}

/** Short footer suffix describing the TLS pin state. */
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

async function openSettingsMenu(
  pi: ExtensionAPI,
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
): Promise<void> {
  const projectTrusted = isAciProjectConfigApproved(ctx);
  const homeDraft = loadHomeAciCloudConfig(os.homedir());
  const drafts: Record<AciConfigScope, AciCloudConfig> = {
    project: projectTrusted ? loadProjectAciCloudConfig(ctx.cwd) : homeDraft,
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
      list.updateValue("autoFetchReceipt", drafts[scope].verify.autoFetchReceipt ? "true" : "false");
      list.updateValue("failOpenOnUnpinned", drafts[scope].verify.failOpenOnUnpinned ? "true" : "false");
      list.updateValue("pinning", drafts[scope].pinning.enabled ? "true" : "false");
    };

    const save = () => {
      if (scope === "project" && !projectTrusted) {
        ctx.ui.notify("Project config cannot be saved until the project is trusted.", "warning");
        return;
      }
      try {
        if (scope === "project") saveProjectAciCloudConfig(ctx.cwd, drafts[scope]);
        else saveHomeAciCloudConfig(os.homedir(), drafts[scope]);
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
        drafts[scope].models.thinkingFormat = newValue as AciCloudConfig["models"]["thinkingFormat"];
        list.updateValue(id, newValue);
        save();
        return;
      }
      if (id === "autoFetchReceipt") {
        drafts[scope].verify.autoFetchReceipt = newValue === "true";
        list.updateValue(id, newValue);
        save();
        return;
      }
      if (id === "failOpenOnUnpinned") {
        drafts[scope].verify.failOpenOnUnpinned = newValue === "true";
        list.updateValue(id, newValue);
        save();
        return;
      }
      if (id === "pinning") {
        drafts[scope].pinning.enabled = newValue === "true";
        list.updateValue(id, newValue);
        save();
        return;
      }
    };

    const scopeItem: SettingItem = {
      id: "scope",
      label: "Config scope",
      description: projectTrusted
        ? formatScopeDescription(scope, ctx.cwd)
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
      {
        id: "autoFetchReceipt",
        label: "Auto-verify receipts",
        description: "Fetch the receipt + attestation after each response",
        currentValue: drafts[scope].verify.autoFetchReceipt ? "true" : "false",
        values: ["true", "false"],
      },
      {
        id: "failOpenOnUnpinned",
        label: "Fail open when unpinned",
        description: "Off (default): block inference until an attested TLS pin is established. On: run unpinned with a footer warning when the attestation is unreachable.",
        currentValue: drafts[scope].verify.failOpenOnUnpinned ? "true" : "false",
        values: ["true", "false"],
      },
      {
        id: "pinning",
        label: "Attested TLS pinning",
        description: "Require the gateway TLS connection to present the attested SPKI (fails closed on mismatch)",
        currentValue: drafts[scope].pinning.enabled ? "true" : "false",
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
          truncateToWidth(theme.fg("accent", theme.bold(settingsTitle())), width),
          "",
          truncateToWidth(modelRegistrationSummary(drafts[scope]), width),
          truncateToWidth(verifySummary(drafts[scope]), width),
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

async function runAttestationCommand(
  ctx: ExtensionCommandContext,
  state: AciRuntimeState,
): Promise<void> {
  const apiKey = resolveApiKey();
  if (!apiKey) {
    ctx.ui.notify(`${API_KEY_ENV} not set`, "error");
    return;
  }
  const attested = await state.store.getAttestation(apiKey, state.config);
  if (!attested) {
    const error = state.store.lastAttestationError ?? "unknown error";
    ctx.ui.notify(`Attestation validation failed: ${error}`, "error");
    return;
  }
  const report = attested.report;
  const verification = attested.verification;
  const keyset = report.attestation?.workload_keyset as WorkloadKeyset | undefined;
  const e2eeKeys = Array.isArray(keyset?.e2ee_public_keys)
    ? (keyset!.e2ee_public_keys as Array<{ key_id?: unknown; algo?: unknown; public_key?: unknown }>)
    : [];
  const receiptKeys = Array.isArray(keyset?.receipt_signing_keys)
    ? (keyset!.receipt_signing_keys as Array<{ key_id?: unknown; algo?: unknown; public_key?: unknown }>)
    : [];
  const notAfter = keyset && typeof keyset.not_after === "number" ? keyset.not_after : undefined;
  const keySummary = (keys: Array<{ key_id?: unknown; algo?: unknown }>) =>
    keys.length === 0
      ? "none"
      : keys.map((k) => `${String(k.key_id)} (${String(k.algo)})`).join(", ");
  const lines = [
    `Aci Cloud attestation`,
    `API version: ${String(report.api_version)}`,
    `Keyset digest: ${verification.workloadKeysetDigest ?? "(unestablished)"}`,
    `Report binding: ${verification.ok ? "verified" : "failed"}`,
    `Keyset not_after: ${notAfter !== undefined ? new Date(notAfter * 1000).toISOString() : "unknown"}`,
    `Encryption keys (${e2eeKeys.length}): ${keySummary(e2eeKeys)}`,
    `Receipt signing keys (${receiptKeys.length}): ${keySummary(receiptKeys)}`,
    `Last receipt: ${state.store.snapshot().receiptId ?? "none"}`,
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
  applyProviderProfile(profileOverride);
  return async (pi: ExtensionAPI) => {
    const cwd = process.cwd();
    const config = loadAciCloudConfig(
      { cwd, home: os.homedir(), includeProject: false },
      overrides,
    );
    const apiKey = resolveApiKey();
    const discovered = apiKey
      ? await discoverAciModels(apiKey, config)
      : { models: fallbackModels(), raw: [] };

    const state: AciRuntimeState = {
      cwd,
      config,
      projectTrusted: false,
      rawModels: discovered.raw,
      store: new AciReceiptStore(),
      overrides,
    };
    registerAciProvider(pi, state);

    pi.on("session_start", async (_event, ctx) => {
      const projectTrusted = isAciProjectConfigApproved(ctx);
      applyEffectiveConfig(pi, state, ctx.cwd, projectTrusted);
      // Resolve the attested SPKI from a fresh report and pin TLS for this
      // session (fail-open; footer shows "UNPINNED" if it cannot be done).
      await installAttestedTlsPin(state);
      updateFooter(ctx, state);
    });

    pi.on("after_provider_response", (event, ctx) => {
      if (ctx.model?.provider !== PROVIDER_ID) return;
      state.store.recordResponseHeaders(event.headers);
      updateFooter(ctx, state);
    });

    pi.on("message_end", (event, ctx) => {
      if (ctx.model?.provider !== PROVIDER_ID) return;
      if (event.message.role !== "assistant") return;
      const key = resolveApiKey();
      if (!key || !state.config.verify.autoFetchReceipt) return;
      void (async () => {
        try {
          await state.store.classifyLastResponse(key, state.config);
        } catch (error) {
          console.error(`${LOG_PREFIX} receipt classification failed:`, error);
        }
        updateFooter(ctx, state);
      })();
    });

    const settingsCommand = `${PROVIDER_ID}-settings`;
    pi.registerCommand(settingsCommand, {
      description: `Configure ${getLabel()} (models, thinking, TLS pinning, verification)`,
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
  };
}

export default createProvider();

export { PROVIDER_ID, PROVIDER_VERSION };
export { profile as getProviderProfile } from "./src/profile.ts";
export { loadAciCloudConfig } from "./src/config.ts";
export { discoverAciModels, mapAciServerModel, inferThinkingFormat } from "./src/models.ts";
// Verification is provided by the reference verifier via the aci-client shim
// (which wraps @phala/aci-verifier). Re-export the pieces callers need.
export {
  type AttestationReport,
  type ReceiptEnvelope,
  type WorkloadKeyset,
  bindAttestation,
  canonicalRequestBytes,
  classifyReceipt,
  fetchAttestation,
  fetchReceipt,
  fetchSession,
  isFullyVerified,
  keysetStaleAfterMs,
  newNonce,
  receiptSigningKeys,
} from "./src/verify.ts";
export {
  verifyReceipt,
  verifyReportBinding,
} from "@phala/aci-verifier";
