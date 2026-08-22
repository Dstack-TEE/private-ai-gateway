// Configuration for the ACI opencode provider.
//
// Layers, lowest to highest precedence:
//   default -> plugin options (opencode.json `"plugin": [["pkg", {...}]]`)
//           -> env (<PREFIX>_* variables, or brand aliases)
//           -> runtime (programmatic override via createProvider(profile, patch))
//
// Unlike the pi provider there is no home/project file layering: opencode
// already merges global + project opencode.json, and provider-level settings
// a user writes into config.provider.<id>.options are respected by the config
// hook (user values win over plugin defaults for standard AI-SDK options).

import { getEnvBaseUrl } from "./constants.ts";
import { profile } from "./profile.ts";

export type AciConfigSource = "runtime" | "env" | "plugin" | "default";

export interface AciModelsConfig {
  /** Only register models whose /v1/models entry has is_tee === true. */
  isTeeOnly: boolean;
  /** Optional model-id allowlist. When set, only these ids are registered. */
  allowlist?: string[];
}

export interface AciVerifyConfig {
  /** Automatically fetch + verify the receipt after each response. */
  autoFetchReceipt: boolean;
  /** Require a cached attestation whose workload matches the receipt. */
  requireAttestationMatch: boolean;
  /** When true, an unpinnable session runs unpinned with a status warning
   *  (fail-open). When false (default) an unpinned session blocks inference
   *  with a clear error rather than silently downgrading to CA-TLS. */
  failOpenOnUnpinned: boolean;
}

export interface AciTlsPinningConfig {
  /** Require the gateway's TLS connection to present the attested SPKI
   *  (fetched from a validated attestation report). Fail closed on mismatch. */
  enabled: boolean;
}

export interface AciCloudConfig {
  baseUrl: string;
  models: AciModelsConfig;
  verify: AciVerifyConfig;
  pinning: AciTlsPinningConfig;
}

export type AciCloudConfigPatch = {
  baseUrl?: unknown;
  models?: Partial<{
    isTeeOnly: unknown;
    allowlist: unknown;
  }>;
  verify?: Partial<{
    autoFetchReceipt: unknown;
    requireAttestationMatch: unknown;
    failOpenOnUnpinned: unknown;
  }>;
  pinning?: Partial<{ enabled: unknown }>;
};

export const DEFAULT_ACI_CLOUD_CONFIG: AciCloudConfig = {
  // Resolved lazily: the profile default enters in loadAciCloudConfig (the
  // profile is applied at factory time, after module evaluation).
  baseUrl: "",
  models: {
    isTeeOnly: true,
  },
  verify: {
    autoFetchReceipt: true,
    requireAttestationMatch: false,
    failOpenOnUnpinned: false,
  },
  pinning: {
    enabled: true,
  },
};

export interface LoadAciCloudConfigOptions {
  env?: NodeJS.ProcessEnv;
  /** Plugin options from `"plugin": [["@phala/...", {...}]]` in opencode.json. */
  pluginOptions?: Record<string, unknown>;
  /** Runtime patch passed to createProvider(). */
  overrides?: AciCloudConfigPatch;
}

function asBoolean(value: unknown): boolean | undefined {
  if (typeof value === "boolean") return value;
  if (typeof value === "string") {
    const v = value.trim().toLowerCase();
    if (v === "true" || v === "1" || v === "yes") return true;
    if (v === "false" || v === "0" || v === "no") return false;
  }
  return undefined;
}

function asStringArray(value: unknown): string[] | undefined {
  if (Array.isArray(value)) {
    const list = value.filter((v): v is string => typeof v === "string" && v.length > 0);
    return list.length > 0 ? list : undefined;
  }
  if (typeof value === "string") {
    const list = value
      .split(",")
      .map((v) => v.trim())
      .filter((v) => v.length > 0);
    return list.length > 0 ? list : undefined;
  }
  return undefined;
}

function asBaseUrl(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed.replace(/\/+$/, "") : undefined;
}

/** Apply one patch-shaped layer (plugin options or runtime overrides) onto a
 *  draft config. Unknown/invalid values are ignored, never partially applied. */
function applyPatch(draft: AciCloudConfig, patch: AciCloudConfigPatch | undefined): void {
  if (!patch) return;
  const baseUrl = asBaseUrl(patch.baseUrl);
  if (baseUrl) draft.baseUrl = baseUrl;

  const isTeeOnly = asBoolean(patch.models?.isTeeOnly);
  if (isTeeOnly !== undefined) draft.models.isTeeOnly = isTeeOnly;
  const allowlist = asStringArray(patch.models?.allowlist);
  if (allowlist) draft.models.allowlist = allowlist;

  const autoFetchReceipt = asBoolean(patch.verify?.autoFetchReceipt);
  if (autoFetchReceipt !== undefined) draft.verify.autoFetchReceipt = autoFetchReceipt;
  const requireAttestationMatch = asBoolean(patch.verify?.requireAttestationMatch);
  if (requireAttestationMatch !== undefined)
    draft.verify.requireAttestationMatch = requireAttestationMatch;
  const failOpenOnUnpinned = asBoolean(patch.verify?.failOpenOnUnpinned);
  if (failOpenOnUnpinned !== undefined) draft.verify.failOpenOnUnpinned = failOpenOnUnpinned;

  const pinningEnabled = asBoolean(patch.pinning?.enabled);
  if (pinningEnabled !== undefined) draft.pinning.enabled = pinningEnabled;
}

/** Plugin options use flat keys for ergonomics in opencode.json:
 *  `{ "baseUrl": "...", "isTeeOnly": false, "failOpenOnUnpinned": true, ... }`. */
function patchFromPluginOptions(options: Record<string, unknown> | undefined): AciCloudConfigPatch {
  if (!options) return {};
  return {
    baseUrl: options.baseUrl,
    models: { isTeeOnly: options.isTeeOnly, allowlist: options.allowlist },
    verify: {
      autoFetchReceipt: options.autoFetchReceipt,
      requireAttestationMatch: options.requireAttestationMatch,
      failOpenOnUnpinned: options.failOpenOnUnpinned,
    },
    pinning: { enabled: options.pinning },
  };
}

function patchFromEnv(env: NodeJS.ProcessEnv): AciCloudConfigPatch {
  const prefix = profile().envPrefix;
  const read = (name: string): string | undefined => env[name]?.trim() || undefined;
  return {
    // Only explicit env vars participate in layering here; brand aliases and
    // the profile default are resolved in loadAciCloudConfig.
    baseUrl: env === process.env ? getEnvBaseUrl() : asBaseUrl(read(`${prefix}_BASE_URL`)),
    models: {
      isTeeOnly: read(`${prefix}_IS_TEE_ONLY`),
      allowlist: read(`${prefix}_MODEL_ALLOWLIST`),
    },
    verify: {
      autoFetchReceipt: read(`${prefix}_AUTO_FETCH_RECEIPT`),
      requireAttestationMatch: read(`${prefix}_REQUIRE_ATTESTATION_MATCH`),
      failOpenOnUnpinned: read(`${prefix}_FAIL_OPEN_ON_UNPINNED`),
    },
    pinning: { enabled: read(`${prefix}_PINNING`) },
  };
}

function cloneDefaults(): AciCloudConfig {
  return {
    baseUrl: DEFAULT_ACI_CLOUD_CONFIG.baseUrl,
    models: { ...DEFAULT_ACI_CLOUD_CONFIG.models },
    verify: { ...DEFAULT_ACI_CLOUD_CONFIG.verify },
    pinning: { ...DEFAULT_ACI_CLOUD_CONFIG.pinning },
  };
}

/** Load the effective config: default -> plugin options -> env -> runtime.
 *  When no layer set the base URL, fall back to the brand profile default. */
export function loadAciCloudConfig(options: LoadAciCloudConfigOptions = {}): AciCloudConfig {
  const draft = cloneDefaults();
  applyPatch(draft, patchFromPluginOptions(options.pluginOptions));
  applyPatch(draft, patchFromEnv(options.env ?? process.env));
  applyPatch(draft, options.overrides);
  if (!draft.baseUrl) draft.baseUrl = profile().defaultBaseUrl;
  return draft;
}
