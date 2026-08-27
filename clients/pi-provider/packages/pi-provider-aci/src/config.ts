// Layered configuration for the ACI provider.
//
// Layers, lowest to highest precedence:
//   default  -> home (~/.pi/providers/<id>/config.json)
//            -> project (cwd/.pi/providers/<id>/config.json, gated by
//              project trust)
//            -> env (<PREFIX>_* variables, or brand aliases)
//            -> runtime (programmatic override via createProvider(profile, patch))
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

import type { AciProviderConfig } from "@phala/aci-provider";

import { getBaseUrl } from "./constants.ts";
import { DEFAULT_PROFILE, type ProviderProfile } from "./profile.ts";

export type ThinkingFormat = "auto" | "qwen" | "openai" | "off";

export interface AciModelsConfig {
  /** Only register models whose /v1/models entry has is_tee === true. */
  isTeeOnly: boolean;
  /** How to map pi thinking levels onto provider request parameters. */
  thinkingFormat: ThinkingFormat;
  /** Optional model-id allowlist. When set, only these ids are registered. */
  allowlist?: string[];
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
}

export function toAciProviderConfig(config: AciCloudConfig): AciProviderConfig {
  return {
    baseURL: config.baseUrl,
    models: config.models,
    trust: config.trust,
    receipts: { verification: "response", historySize: 32 },
  };
}

export type AciCloudConfigPatch = {
  baseUrl?: unknown;
  models?: Partial<{
    isTeeOnly: unknown;
    thinkingFormat: unknown;
    allowlist: unknown;
  }>;
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
    thinkingFormat: "auto",
  },
  trust: {},
};

/** Profile default config with the base URL resolved from the supplied env. */
function defaultAciCloudConfig(
  providerProfile: ProviderProfile,
  env: NodeJS.ProcessEnv,
): AciCloudConfig {
  return {
    ...DEFAULT_ACI_CLOUD_CONFIG,
    baseUrl: getBaseUrl(providerProfile, env) || DEFAULT_ACI_CLOUD_CONFIG.baseUrl,
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

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function mergeConfigPatch<T extends Record<string, unknown>>(
  base: T,
  patch: Record<string, unknown>,
): T {
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
  return result as T;
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

function readConfigFileQuiet(path: string, logPrefix: string): Record<string, unknown> {
  try {
    return readConfigFile(path);
  } catch (error) {
    console.error(`${logPrefix} failed to read config file ${path}:`, error);
    return {};
  }
}

function parseBoolean(value: string | undefined): boolean | undefined {
  if (value === undefined) return undefined;
  const trimmed = value.trim().toLowerCase();
  if (trimmed === "true" || trimmed === "1") return true;
  if (trimmed === "false" || trimmed === "0") return false;
  return undefined;
}

function envConfigPatch(
  env: NodeJS.ProcessEnv,
  providerProfile: ProviderProfile,
): AciCloudConfigPatch {
  const patch: AciCloudConfigPatch = {};
  const prefix = providerProfile.envPrefix;
  const read = (...names: string[]) => {
    for (const name of names) {
      const v = env[name]?.trim();
      if (v) return v;
    }
    return undefined;
  };

  const baseUrl = read(
    `${prefix}_CLOUD_API_PREFIX`,
    `${prefix}_BASE_URL`,
    `${prefix}_CLOUD_BASE_URL`,
  );
  if (baseUrl) patch.baseUrl = baseUrl;

  const isTeeOnly = parseBoolean(read(`${prefix}_IS_TEE_ONLY`, `${prefix}_TEE_ONLY`) ?? undefined);
  if (isTeeOnly !== undefined) patch.models = { ...patch.models, isTeeOnly };

  const thinkingFormat = read(`${prefix}_THINKING_FORMAT`);
  if (thinkingFormat) patch.models = { ...patch.models, thinkingFormat };

  const acceptedComposeHashes = read(`${prefix}_ACCEPTED_COMPOSE_HASHES`);
  if (acceptedComposeHashes) {
    patch.trust = {
      acceptedComposeHashes: acceptedComposeHashes
        .split(",")
        .map((hash) => hash.trim())
        .filter(Boolean),
    };
  }

  const acceptedSessionIds = read(`${prefix}_ACCEPTED_SESSION_IDS`);
  if (acceptedSessionIds) {
    patch.trust = {
      ...patch.trust,
      acceptedSessionIds: acceptedSessionIds
        .split(",")
        .map((id) => id.trim())
        .filter(Boolean),
    };
  }

  return patch;
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

function requireString(raw: unknown, configPath: string, pointer: string): string {
  if (typeof raw === "string" && raw.length > 0) return raw;
  return fail(configPath, pointer, `expected a non-empty string, got ${JSON.stringify(raw)}`);
}

function requireBoolean(raw: unknown, configPath: string, pointer: string): boolean {
  if (typeof raw === "boolean") return raw;
  return fail(configPath, pointer, `expected a boolean, got ${JSON.stringify(raw)}`);
}

function requireThinkingFormat(raw: unknown, configPath: string, pointer: string): ThinkingFormat {
  if (raw === "auto" || raw === "qwen" || raw === "openai" || raw === "off") return raw;
  return fail(
    configPath,
    pointer,
    `expected "auto" | "qwen" | "openai" | "off", got ${JSON.stringify(raw)}`,
  );
}

function requireStringArray(
  raw: unknown,
  configPath: string,
  pointer: string,
): string[] | undefined {
  if (raw === undefined || raw === null) return undefined;
  if (!Array.isArray(raw)) {
    return fail(configPath, pointer, `expected an array, got ${typeof raw}`);
  }
  return raw.map((value, index) => {
    if (typeof value !== "string" || value.length === 0) {
      return fail(
        configPath,
        `${pointer}/${index}`,
        `expected a non-empty string, got ${JSON.stringify(value)}`,
      );
    }
    return value;
  });
}

function validateModelsConfig(raw: unknown, configPath: string, pointer: string): AciModelsConfig {
  const model = requireRecord(raw, configPath, pointer);
  return {
    isTeeOnly: requireBoolean(model.isTeeOnly, configPath, `${pointer}/isTeeOnly`),
    thinkingFormat: requireThinkingFormat(
      model.thinkingFormat,
      configPath,
      `${pointer}/thinkingFormat`,
    ),
    allowlist: requireStringArray(model.allowlist, configPath, `${pointer}/allowlist`),
  };
}

function validateTrustConfig(
  raw: unknown,
  configPath: string,
  pointer: string,
): AciCloudConfig["trust"] {
  const trust = requireRecord(raw, configPath, pointer);
  const acceptedComposeHashes = requireStringArray(
    trust.acceptedComposeHashes,
    configPath,
    `${pointer}/acceptedComposeHashes`,
  );
  const acceptedSessionIds = requireStringArray(
    trust.acceptedSessionIds,
    configPath,
    `${pointer}/acceptedSessionIds`,
  );
  for (const [name, values] of [
    ["acceptedComposeHashes", acceptedComposeHashes],
    ["acceptedSessionIds", acceptedSessionIds],
  ] as const) {
    if (values !== undefined && values.length === 0) {
      fail(configPath, `${pointer}/${name}`, "expected a non-empty array when supplied");
    }
  }
  for (const [index, hash] of (acceptedComposeHashes ?? []).entries()) {
    if (!/^[0-9a-f]{64}$/i.test(hash)) {
      fail(
        configPath,
        `${pointer}/acceptedComposeHashes/${index}`,
        "expected a 64-character SHA-256 hex digest",
      );
    }
  }
  for (const [index, id] of (acceptedSessionIds ?? []).entries()) {
    if (!/^[0-9a-f]{64}$/.test(id)) {
      fail(
        configPath,
        `${pointer}/acceptedSessionIds/${index}`,
        "expected a 64-character lowercase session id",
      );
    }
  }
  return {
    ...(acceptedComposeHashes === undefined
      ? {}
      : { acceptedComposeHashes: acceptedComposeHashes.map((hash) => hash.toLowerCase()) }),
    ...(acceptedSessionIds === undefined ? {} : { acceptedSessionIds }),
  };
}

export function validateAciCloudConfig(raw: unknown, configPath = "<aci-config>"): AciCloudConfig {
  const config = requireRecord(raw, configPath, "");
  return {
    baseUrl: requireString(config.baseUrl, configPath, "/baseUrl"),
    models: validateModelsConfig(config.models, configPath, "/models"),
    trust: validateTrustConfig(config.trust, configPath, "/trust"),
  };
}

function loadLayers(
  options: LoadAciCloudConfigOptions,
  overrides?: AciCloudConfigPatch,
): Record<string, unknown>[] {
  const providerProfile = options.profile ?? DEFAULT_PROFILE;
  const layers: Record<string, unknown>[] = [
    readConfigFile(getGlobalAciCloudConfigPath(options.home, providerProfile.providerId)),
  ];
  if (options.includeProject !== false) {
    layers.push(
      readConfigFile(getProjectAciCloudConfigPath(options.cwd, providerProfile.providerId)),
    );
  }
  layers.push(
    envConfigPatch(options.env ?? process.env, providerProfile) as Record<string, unknown>,
  );
  if (overrides) {
    layers.push(overrides as Record<string, unknown>);
  }
  return layers;
}

export function loadAciCloudConfig(
  options: LoadAciCloudConfigOptions,
  overrides?: AciCloudConfigPatch,
): AciCloudConfig {
  const providerProfile = options.profile ?? DEFAULT_PROFILE;
  let merged = clone(
    defaultAciCloudConfig(providerProfile, options.env ?? process.env),
  ) as unknown as Record<string, unknown>;
  for (const layer of loadLayers(options, overrides)) {
    merged = mergeConfigPatch(merged, layer);
  }
  return validateAciCloudConfig(merged);
}

export function loadProjectAciCloudConfig(
  cwd: string,
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
  env: NodeJS.ProcessEnv = process.env,
): AciCloudConfig {
  return validateAciCloudConfig(
    mergeConfigPatch(
      clone(defaultAciCloudConfig(providerProfile, env)) as unknown as Record<string, unknown>,
      readConfigFileQuiet(
        getProjectAciCloudConfigPath(cwd, providerProfile.providerId),
        providerProfile.logPrefix,
      ),
    ),
  );
}

export function loadHomeAciCloudConfig(
  home: string,
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
  env: NodeJS.ProcessEnv = process.env,
): AciCloudConfig {
  return validateAciCloudConfig(
    mergeConfigPatch(
      clone(defaultAciCloudConfig(providerProfile, env)) as unknown as Record<string, unknown>,
      readConfigFileQuiet(
        getGlobalAciCloudConfigPath(home, providerProfile.providerId),
        providerProfile.logPrefix,
      ),
    ),
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
