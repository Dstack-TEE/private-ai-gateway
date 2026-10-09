// Layered configuration for the ACI provider.
//
// Layers, lowest to highest precedence:
//   default  -> home (~/.pi/providers/<id>/config.json)
//            -> project (cwd/.pi/providers/<id>/config.json, gated by
//              project trust)
//            -> env (<PREFIX>_* variables, or brand aliases)
//            -> runtime (programmatic override via createProvider({ config }))
//
// Validation runs after merge so a malformed layer never produces a
// partially-applied config.

import {
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";

import {
  aciProviderConfigInputFromEnv,
  AciProviderConfigError,
  resolveAciProviderConfig,
  type AciProviderConfig,
} from "@phala/aci-provider/config";

import { DEFAULT_PROFILE, type ProviderProfile } from "./profile.ts";
import type { ModelCompatOverride } from "./models.ts";

const THINKING_LEVELS = ["off", "minimal", "low", "medium", "high", "xhigh", "max"] as const;

export interface AciModelsConfig {
  /** Only register models whose /v1/models entry has is_tee === true. */
  isTeeOnly: boolean;
  /** Optional model-id allowlist. When set, only these ids are registered. */
  allowlist?: string[];
  /**
   * Per-model compatibility patches over the builtin override table (known
   * upstream quirks). Keyed by catalog model id.
   */
  overrides?: Record<string, ModelCompatOverride>;
}

/**
 * Receipt verification policy. "response" verifies every inference receipt
 * before the turn can complete (fail-closed, the ACI default). "on-demand"
 * still records receipts and keeps them auditable via /<provider>-receipt,
 * but never blocks a response — an operator escape hatch, not a user
 * comfort switch.
 */
export type ReceiptVerificationMode = "response" | "on-demand";

export interface AciReceiptsConfig {
  verification: ReceiptVerificationMode;
}

export interface AciCloudConfig {
  baseUrl: string;
  models: AciModelsConfig;
  trust: {
    /** RTMR3-bound compose hashes reviewed by the operator or brand. */
    acceptedComposeHashes?: string[];
    /** Attested upstream session ids accepted by the operator or brand. */
    acceptedSessionIds?: string[];
  };
  receipts: AciReceiptsConfig;
}

export function toAciProviderConfig(config: AciCloudConfig): AciProviderConfig {
  return {
    baseURL: config.baseUrl,
    models: config.models,
    trust: config.trust,
    // Tolerate hand-built partial configs (public API consumers construct
    // AciCloudConfig literals without the defaulted receipts field).
    receipts: {
      verification: config.receipts?.verification ?? "response",
      historySize: 32,
    },
  };
}

export type AciCloudConfigPatch = {
  baseUrl?: unknown;
  models?: Partial<{
    isTeeOnly: unknown;
    allowlist: unknown;
    overrides: unknown;
  }>;
  receipts?: unknown;
  trust?: Partial<{
    acceptedComposeHashes: unknown;
    acceptedSessionIds: unknown;
  }>;
};

export interface LoadAciCloudConfigOptions {
  cwd: string;
  home: string;
  env?: NodeJS.ProcessEnv;
  includeProject?: boolean;
  profile?: ProviderProfile;
}

export const PI_CONFIG_DIR_NAME = ".pi";

export class ConfigError extends Error {
  public readonly configPath: string;
  public readonly pointer?: string;

  constructor(message: string, configPath: string, pointer?: string) {
    super(pointer ? `${configPath}${pointer}: ${message}` : `${configPath}: ${message}`);
    this.name = "ConfigError";
    this.configPath = configPath;
    this.pointer = pointer;
  }
}

export const DEFAULT_ACI_CLOUD_CONFIG: AciCloudConfig = {
  baseUrl: DEFAULT_PROFILE.defaultBaseURL,
  models: {
    isTeeOnly: true,
  },
  trust: {},
  receipts: { verification: "response" },
};

function defaultAciCloudConfig(providerProfile: ProviderProfile): AciCloudConfig {
  return {
    ...DEFAULT_ACI_CLOUD_CONFIG,
    baseUrl: providerProfile.defaultBaseURL,
    trust: {
      ...(providerProfile.acceptedComposeHashes === undefined
        ? {}
        : { acceptedComposeHashes: [...providerProfile.acceptedComposeHashes] }),
      ...(providerProfile.acceptedSessionIds === undefined
        ? {}
        : { acceptedSessionIds: [...providerProfile.acceptedSessionIds] }),
    },
  };
}

export function getGlobalAciCloudConfigPath(
  home: string,
  providerId = DEFAULT_PROFILE.providerId,
): string {
  return join(home, PI_CONFIG_DIR_NAME, "providers", providerId, "config.json");
}

export function getProjectAciCloudConfigPath(
  cwd: string,
  providerId = DEFAULT_PROFILE.providerId,
): string {
  return join(cwd, PI_CONFIG_DIR_NAME, "providers", providerId, "config.json");
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function mergeConfigPatch(base: object, patch: object): Record<string, unknown> {
  const result: Record<string, unknown> = { ...base };
  for (const [key, value] of Object.entries(patch)) {
    if (value === undefined) continue;
    const current = result[key];
    if (isRecord(current) && isRecord(value)) {
      result[key] = mergeConfigPatch(current, value);
    } else {
      result[key] = value;
    }
  }
  return result;
}

function readConfigFile(path: string): Record<string, unknown> {
  if (!existsSync(path)) return {};
  let contents: string;
  try {
    contents = readFileSync(path, "utf8");
  } catch (error) {
    throw new ConfigError(
      `failed to read config: ${error instanceof Error ? error.message : String(error)}`,
      path,
    );
  }
  try {
    const parsed = JSON.parse(contents) as unknown;
    if (isRecord(parsed)) return parsed;
    throw new ConfigError("config file must be a JSON object", path);
  } catch (error) {
    if (error instanceof ConfigError) throw error;
    throw new ConfigError(
      `invalid JSON: ${error instanceof Error ? error.message : String(error)}`,
      path,
    );
  }
}

function envConfigPatch(
  env: NodeJS.ProcessEnv,
  providerProfile: ProviderProfile,
): AciCloudConfigPatch {
  const input = aciProviderConfigInputFromEnv(providerProfile, env);
  const verification = env[`${providerProfile.envPrefix}_RECEIPTS_VERIFICATION`];
  return {
    ...(input.baseURL !== undefined ? { baseUrl: input.baseURL } : {}),
    ...(input.models ? { models: input.models } : {}),
    ...(input.trust ? { trust: input.trust } : {}),
    ...(verification !== undefined ? { receipts: { verification } } : {}),
  };
}

function fail(configPath: string, pointer: string, message: string): never {
  throw new ConfigError(message, configPath, pointer);
}

function requireRecord(raw: unknown, configPath: string, pointer: string): Record<string, unknown> {
  if (isRecord(raw)) return raw;
  return fail(
    configPath,
    pointer,
    `expected an object, got ${Array.isArray(raw) ? "array" : typeof raw}`,
  );
}

/** Validate the models.overrides record; unknown model ids are allowed. */
function validateModelOverrides(
  raw: unknown,
  configPath: string,
): Record<string, ModelCompatOverride> | undefined {
  if (raw === undefined) return undefined;
  const record = requireRecord(raw, configPath, "/models/overrides");
  const result: Record<string, ModelCompatOverride> = {};
  for (const [modelId, patch] of Object.entries(record)) {
    const pointer = `/models/overrides/${modelId}`;
    const p = requireRecord(patch, configPath, pointer);
    const override: ModelCompatOverride = {};
    if (p.supportsDeveloperRole !== undefined) {
      if (typeof p.supportsDeveloperRole !== "boolean") {
        fail(configPath, `${pointer}/supportsDeveloperRole`, "expected a boolean");
      }
      override.supportsDeveloperRole = p.supportsDeveloperRole;
    }
    if (p.maxTokens !== undefined) {
      if (typeof p.maxTokens !== "number" || !Number.isInteger(p.maxTokens) || p.maxTokens <= 0) {
        fail(configPath, `${pointer}/maxTokens`, "expected a positive integer");
      }
      override.maxTokens = p.maxTokens;
    }
    if (p.thinkingLevelMap !== undefined) {
      const map = requireRecord(p.thinkingLevelMap, configPath, `${pointer}/thinkingLevelMap`);
      const levelMap: ModelCompatOverride["thinkingLevelMap"] = {};
      for (const [level, value] of Object.entries(map)) {
        if (!(THINKING_LEVELS as readonly string[]).includes(level)) {
          fail(
            configPath,
            `${pointer}/thinkingLevelMap/${level}`,
            `unknown thinking level (expected one of: ${THINKING_LEVELS.join(", ")})`,
          );
        }
        if (value !== null && typeof value !== "string") {
          fail(configPath, `${pointer}/thinkingLevelMap/${level}`, "expected a string or null");
        }
        levelMap[level as keyof typeof levelMap] = value;
      }
      override.thinkingLevelMap = levelMap;
    }
    result[modelId] = override;
  }
  return result;
}

export function validateAciCloudConfig(
  raw: unknown,
  configPath = "<aci-config>",
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
): AciCloudConfig {
  const config = requireRecord(raw, configPath, "");
  const models = requireRecord(config.models, configPath, "/models");
  const trust = requireRecord(config.trust, configPath, "/trust");
  // Absent records fall through to the required-field loop below so the
  // error names the missing pointer precisely (e.g. /baseUrl) instead of
  // whichever requireRecord ran first.
  const receipts =
    config.receipts === undefined ? {} : requireRecord(config.receipts, configPath, "/receipts");
  for (const [record, field, pointer] of [
    [config, "baseUrl", "/baseUrl"],
    [models, "isTeeOnly", "/models/isTeeOnly"],
    [receipts, "verification", "/receipts/verification"],
  ] as const) {
    if (!(field in record)) fail(configPath, pointer, "required field is missing");
  }
  if (receipts.verification !== "response" && receipts.verification !== "on-demand") {
    fail(configPath, "/receipts/verification", 'expected "response" or "on-demand"');
  }
  try {
    const resolved = resolveAciProviderConfig(
      providerProfile,
      {
        baseURL: config.baseUrl,
        models: {
          isTeeOnly: models.isTeeOnly,
          allowlist: models.allowlist,
        },
        trust: {
          acceptedComposeHashes: trust.acceptedComposeHashes,
          acceptedSessionIds: trust.acceptedSessionIds,
        },
        receipts: { verification: "response", historySize: 32 },
      },
      {},
    );
    const overrides = validateModelOverrides(models.overrides, configPath);
    return {
      baseUrl: resolved.baseURL,
      models: {
        isTeeOnly: resolved.models.isTeeOnly,
        ...(resolved.models.allowlist ? { allowlist: [...resolved.models.allowlist] } : {}),
        ...(overrides ? { overrides } : {}),
      },
      receipts: { verification: receipts.verification },
      trust: {
        ...(resolved.trust.acceptedComposeHashes
          ? { acceptedComposeHashes: [...resolved.trust.acceptedComposeHashes] }
          : {}),
        ...(resolved.trust.acceptedSessionIds
          ? { acceptedSessionIds: [...resolved.trust.acceptedSessionIds] }
          : {}),
      },
    };
  } catch (error) {
    if (!(error instanceof AciProviderConfigError)) throw error;
    const pointer = error.pointer === "/baseURL" ? "/baseUrl" : error.pointer;
    const detail = error.message.slice(error.pointer.length + 2);
    return fail(configPath, pointer, detail);
  }
}

function loadLayers(options: LoadAciCloudConfigOptions, overrides?: AciCloudConfigPatch): object[] {
  const providerProfile = options.profile ?? DEFAULT_PROFILE;
  const layers: object[] = [
    readConfigFile(getGlobalAciCloudConfigPath(options.home, providerProfile.providerId)),
  ];
  if (options.includeProject !== false) {
    layers.push(
      readConfigFile(getProjectAciCloudConfigPath(options.cwd, providerProfile.providerId)),
    );
  }
  layers.push(envConfigPatch(options.env ?? process.env, providerProfile));
  if (overrides) {
    layers.push(overrides);
  }
  return layers;
}

export function loadAciCloudConfig(
  options: LoadAciCloudConfigOptions,
  overrides?: AciCloudConfigPatch,
): AciCloudConfig {
  const providerProfile = options.profile ?? DEFAULT_PROFILE;
  let merged: object = defaultAciCloudConfig(providerProfile);
  for (const layer of loadLayers(options, overrides)) {
    merged = mergeConfigPatch(merged, layer);
  }
  return validateAciCloudConfig(merged, "<aci-config>", providerProfile);
}

export function loadProjectAciCloudConfig(
  cwd: string,
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
): AciCloudConfig {
  return validateAciCloudConfig(
    mergeConfigPatch(
      defaultAciCloudConfig(providerProfile),
      readConfigFile(getProjectAciCloudConfigPath(cwd, providerProfile.providerId)),
    ),
    "<aci-config>",
    providerProfile,
  );
}

export function loadHomeAciCloudConfig(
  home: string,
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
): AciCloudConfig {
  return validateAciCloudConfig(
    mergeConfigPatch(
      defaultAciCloudConfig(providerProfile),
      readConfigFile(getGlobalAciCloudConfigPath(home, providerProfile.providerId)),
    ),
    "<aci-config>",
    providerProfile,
  );
}

export function saveProjectAciCloudConfig(
  cwd: string,
  config: AciCloudConfig,
  providerId = DEFAULT_PROFILE.providerId,
): void {
  saveAciCloudConfigFile(getProjectAciCloudConfigPath(cwd, providerId), config);
}

export function saveHomeAciCloudConfig(
  home: string,
  config: AciCloudConfig,
  providerId = DEFAULT_PROFILE.providerId,
): void {
  saveAciCloudConfigFile(getGlobalAciCloudConfigPath(home, providerId), config);
}

function saveAciCloudConfigFile(path: string, config: AciCloudConfig): void {
  mkdirSync(dirname(path), { recursive: true });
  // Atomic write: temp file + rename in the same directory, so a crash or
  // ENOSPC mid-write cannot leave a torn JSON that silently resets settings
  // to defaults on the next read (previously plain writeFileSync).
  const tempPath = `${path}.tmp`;
  try {
    writeFileSync(
      tempPath,
      `${JSON.stringify(validateAciCloudConfig(config, path), null, 2)}\n`,
      "utf8",
    );
    renameSync(tempPath, path);
  } catch (error) {
    try {
      unlinkSync(tempPath);
    } catch {
      // Cleanup is best-effort; the original file (if any) is left untouched.
    }
    throw new ConfigError(
      `failed to write config: ${error instanceof Error ? error.message : String(error)}`,
      path,
    );
  }
}
