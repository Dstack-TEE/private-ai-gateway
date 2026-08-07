// Layered configuration for the ACI provider.
//
// Layers, lowest to highest precedence:
//   default  -> home (~/.pi/providers/<id>/config.json)
//            -> project (cwd/.pi/providers/<id>/config.json, gated by
//              project trust)
//            -> env (<PREFIX>_* variables, or brand aliases)
//            -> runtime (programmatic override via createAciProvider(patch))
//
// Each config value records which layer it came from (sources) so the settings
// UI can show provenance. Validation runs after merge so a malformed layer
// never produces a partially-applied config.

import { existsSync, mkdirSync, readFileSync, renameSync, unlinkSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";

import { DEFAULT_BASE_URL, LOG_PREFIX, PROVIDER_ID, getBaseUrl } from "./constants.ts";
import { profile } from "./profile.ts";

export type AciConfigSource = "runtime" | "env" | "project" | "home" | "default";

export type ThinkingFormat = "auto" | "qwen" | "openai" | "off";

export interface AciModelsConfig {
  /** Only register models whose /v1/models entry has is_tee === true. */
  isTeeOnly: boolean;
  /** How to map pi thinking levels onto provider request parameters. */
  thinkingFormat: ThinkingFormat;
  /** Optional model-id allowlist. When set, only these ids are registered. */
  allowlist?: string[];
}

export interface AciVerifyConfig {
  /** Automatically fetch the receipt after each response and update the footer. */
  autoFetchReceipt: boolean;
  /** Require a cached attestation whose workload matches the receipt. */
  requireAttestationMatch: boolean;
  /** When true, an unpinnable session runs unpinned with a footer warning
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
  /** Default model id to surface first in /model. */
  defaultModel?: string;
}

export type AciCloudConfigPatch = {
  baseUrl?: unknown;
  models?: Partial<{
    isTeeOnly: unknown;
    thinkingFormat: unknown;
    allowlist: unknown;
  }>;
  verify?: Partial<{
    autoFetchReceipt: unknown;
    requireAttestationMatch: unknown;
    failOpenOnUnpinned: unknown;
  }>;
  pinning?: Partial<{ enabled: unknown }>;
  defaultModel?: unknown;
};

export interface AciCloudConfigSources {
  baseUrl: AciConfigSource;
  models: {
    isTeeOnly: AciConfigSource;
    thinkingFormat: AciConfigSource;
    allowlist: AciConfigSource;
  };
  verify: {
    autoFetchReceipt: AciConfigSource;
    requireAttestationMatch: AciConfigSource;
    failOpenOnUnpinned: AciConfigSource;
  };
  pinning: { enabled: AciConfigSource };
  defaultModel: AciConfigSource;
}

export interface LoadAciCloudConfigOptions {
  cwd: string;
  home: string;
  env?: NodeJS.ProcessEnv;
  includeProject?: boolean;
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
  baseUrl: DEFAULT_BASE_URL,
  models: {
    isTeeOnly: true,
    thinkingFormat: "auto",
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


/** Current-profile default config, with the base URL resolved live from the
 *  profile/env (the neutral default may leave it operator-set). */
function defaultAciCloudConfig(): AciCloudConfig {
  return { ...DEFAULT_ACI_CLOUD_CONFIG, baseUrl: getBaseUrl() || DEFAULT_ACI_CLOUD_CONFIG.baseUrl };
}

let runtimeOverride: AciCloudConfigPatch = {};

export function setRuntimeAciCloudConfigOverride(patch: AciCloudConfigPatch): void {
  runtimeOverride = mergeConfigPatch(runtimeOverride, patch);
}

export function getGlobalAciCloudConfigPath(home: string): string {
  return join(home, PI_CONFIG_DIR_NAME, "providers", PROVIDER_ID, "config.json");
}

export function getProjectAciCloudConfigPath(cwd: string): string {
  return join(cwd, PI_CONFIG_DIR_NAME, "providers", PROVIDER_ID, "config.json");
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

function readConfigFileQuiet(path: string): Record<string, unknown> {
  try {
    return readConfigFile(path);
  } catch (error) {
    console.error(`${LOG_PREFIX} failed to read config file ${path}:`, error);
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

function envConfigPatch(env: NodeJS.ProcessEnv): AciCloudConfigPatch {
  const patch: AciCloudConfigPatch = {};
  const p = profile();
  const prefix = p.envPrefix;
  const read = (...names: string[]) => {
    for (const name of names) {
      const v = env[name]?.trim();
      if (v) return v;
    }
    return undefined;
  };

  const baseUrl = read(`${prefix}_CLOUD_API_PREFIX`, `${prefix}_BASE_URL`, `${prefix}_CLOUD_BASE_URL`);
  if (baseUrl) patch.baseUrl = baseUrl;

  const isTeeOnly = parseBoolean(read(`${prefix}_IS_TEE_ONLY`, `${prefix}_TEE_ONLY`) ?? undefined);
  if (isTeeOnly !== undefined) patch.models = { ...patch.models, isTeeOnly };

  const thinkingFormat = read(`${prefix}_THINKING_FORMAT`);
  if (thinkingFormat) patch.models = { ...patch.models, thinkingFormat };

  const autoFetch = parseBoolean(read(`${prefix}_AUTO_VERIFY`) ?? undefined);
  if (autoFetch !== undefined) patch.verify = { ...patch.verify, autoFetchReceipt: autoFetch };

  const defaultModel = read(`${prefix}_DEFAULT_MODEL`);
  if (defaultModel) patch.defaultModel = defaultModel;

  const tlsPinning = parseBoolean(read(`${prefix}_TLS_PINNING`) ?? undefined);
  if (tlsPinning !== undefined) patch.pinning = { enabled: tlsPinning };

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

function requireOptionalString(
  raw: unknown,
  configPath: string,
  pointer: string,
): string | undefined {
  if (raw === undefined || raw === null) return undefined;
  if (typeof raw === "string" && raw.length > 0) return raw;
  return fail(configPath, pointer, `expected a non-empty string, got ${JSON.stringify(raw)}`);
}

function requireBoolean(raw: unknown, configPath: string, pointer: string): boolean {
  if (typeof raw === "boolean") return raw;
  return fail(configPath, pointer, `expected a boolean, got ${JSON.stringify(raw)}`);
}

function requireThinkingFormat(
  raw: unknown,
  configPath: string,
  pointer: string,
): ThinkingFormat {
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

function validateModelsConfig(
  raw: unknown,
  configPath: string,
  pointer: string,
): AciModelsConfig {
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

function validateVerifyConfig(
  raw: unknown,
  configPath: string,
  pointer: string,
): AciVerifyConfig {
  const verify = requireRecord(raw, configPath, pointer);
  return {
    autoFetchReceipt: requireBoolean(
      verify.autoFetchReceipt,
      configPath,
      `${pointer}/autoFetchReceipt`,
    ),
    requireAttestationMatch: requireBoolean(
      verify.requireAttestationMatch,
      configPath,
      `${pointer}/requireAttestationMatch`,
    ),
    failOpenOnUnpinned: requireBoolean(
      verify.failOpenOnUnpinned,
      configPath,
      `${pointer}/failOpenOnUnpinned`,
    ),
  };
}

function validatePinningConfig(
  raw: unknown,
  configPath: string,
  pointer: string,
): AciTlsPinningConfig {
  const pinning = requireRecord(raw, configPath, pointer);
  return {
    enabled: requireBoolean(pinning.enabled, configPath, `${pointer}/enabled`),
  };
}

export function validateAciCloudConfig(
  raw: unknown,
  configPath = "<aci-config>",
): AciCloudConfig {
  const config = requireRecord(raw, configPath, "");
  return {
    baseUrl: requireString(config.baseUrl, configPath, "/baseUrl"),
    models: validateModelsConfig(config.models, configPath, "/models"),
    verify: validateVerifyConfig(config.verify, configPath, "/verify"),
    pinning: validatePinningConfig(config.pinning, configPath, "/pinning"),
    defaultModel: requireOptionalString(config.defaultModel, configPath, "/defaultModel"),
  };
}

function hasPath(config: Record<string, unknown>, path: readonly string[]): boolean {
  let current: unknown = config;
  for (const key of path) {
    if (!isRecord(current) || !Object.hasOwn(current, key)) return false;
    current = current[key];
  }
  return true;
}

function sourceForPath(
  layers: Array<{ source: AciConfigSource; config: Record<string, unknown> }>,
  path: readonly string[],
): AciConfigSource {
  for (let i = layers.length - 1; i >= 0; i--) {
    if (hasPath(layers[i].config, path)) return layers[i].source;
  }
  return "default";
}

function buildSources(
  layers: Array<{ source: AciConfigSource; config: Record<string, unknown> }>,
): AciCloudConfigSources {
  return {
    baseUrl: sourceForPath(layers, ["baseUrl"]),
    models: {
      isTeeOnly: sourceForPath(layers, ["models", "isTeeOnly"]),
      thinkingFormat: sourceForPath(layers, ["models", "thinkingFormat"]),
      allowlist: sourceForPath(layers, ["models", "allowlist"]),
    },
    verify: {
      autoFetchReceipt: sourceForPath(layers, ["verify", "autoFetchReceipt"]),
      requireAttestationMatch: sourceForPath(layers, ["verify", "requireAttestationMatch"]),
      failOpenOnUnpinned: sourceForPath(layers, ["verify", "failOpenOnUnpinned"]),
    },
    pinning: { enabled: sourceForPath(layers, ["pinning", "enabled"]) },
    defaultModel: sourceForPath(layers, ["defaultModel"]),
  };
}

function loadLayers(
  options: LoadAciCloudConfigOptions,
): Array<{ source: AciConfigSource; config: Record<string, unknown> }> {
  const layers: Array<{ source: AciConfigSource; config: Record<string, unknown> }> = [
    { source: "home", config: readConfigFile(getGlobalAciCloudConfigPath(options.home)) },
  ];
  if (options.includeProject !== false) {
    layers.push({
      source: "project",
      config: readConfigFile(getProjectAciCloudConfigPath(options.cwd)),
    });
  }
  layers.push(
    {
      source: "env",
      config: envConfigPatch(options.env ?? process.env) as Record<string, unknown>,
    },
    { source: "runtime", config: runtimeOverride as Record<string, unknown> },
  );
  return layers;
}

export function loadAciCloudConfig(
  options: LoadAciCloudConfigOptions,
  overrides?: AciCloudConfigPatch,
): AciCloudConfig {
  let merged = clone(defaultAciCloudConfig()) as unknown as Record<string, unknown>;
  for (const layer of loadLayers(options)) {
    merged = mergeConfigPatch(merged, layer.config);
  }
  if (overrides) {
    merged = mergeConfigPatch(merged, overrides as Record<string, unknown>);
  }
  return validateAciCloudConfig(merged);
}

export function loadAciCloudConfigSources(
  options: LoadAciCloudConfigOptions,
): AciCloudConfigSources {
  return buildSources(loadLayers(options));
}

export function loadProjectAciCloudConfig(cwd: string): AciCloudConfig {
  return validateAciCloudConfig(
    mergeConfigPatch(
      clone(defaultAciCloudConfig()) as unknown as Record<string, unknown>,
      readConfigFileQuiet(getProjectAciCloudConfigPath(cwd)),
    ),
  );
}

export function loadHomeAciCloudConfig(home: string): AciCloudConfig {
  return validateAciCloudConfig(
    mergeConfigPatch(
      clone(defaultAciCloudConfig()) as unknown as Record<string, unknown>,
      readConfigFileQuiet(getGlobalAciCloudConfigPath(home)),
    ),
  );
}

export function saveProjectAciCloudConfig(cwd: string, config: AciCloudConfig): void {
  saveAciCloudConfigFile(getProjectAciCloudConfigPath(cwd), config);
}

export function saveHomeAciCloudConfig(home: string, config: AciCloudConfig): void {
  saveAciCloudConfigFile(getGlobalAciCloudConfigPath(home), config);
}

function saveAciCloudConfigFile(path: string, config: AciCloudConfig): void {
  mkdirSync(dirname(path), { recursive: true });
  // Atomic write: temp file + rename in the same directory, so a crash or
  // ENOSPC mid-write cannot leave a torn JSON that silently resets settings
  // to defaults on the next read (previously plain writeFileSync).
  const tempPath = `${path}.tmp`;
  try {
    writeFileSync(tempPath, `${JSON.stringify(validateAciCloudConfig(config, path), null, 2)}\n`, "utf8");
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
