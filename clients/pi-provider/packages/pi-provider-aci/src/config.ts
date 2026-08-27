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

import {
  aciProviderConfigInputFromEnv,
  AciProviderConfigError,
  resolveAciProviderConfig,
  type AciProviderConfig,
} from "@phala/aci-provider";

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

function readConfigFileQuiet(path: string, logPrefix: string): Record<string, unknown> {
  try {
    return readConfigFile(path);
  } catch (error) {
    console.error(`${logPrefix} failed to read config file ${path}:`, error);
    return {};
  }
}

function envConfigPatch(
  env: NodeJS.ProcessEnv,
  providerProfile: ProviderProfile,
): AciCloudConfigPatch {
  const input = aciProviderConfigInputFromEnv(providerProfile, env);
  return {
    ...(input.baseURL !== undefined ? { baseUrl: input.baseURL } : {}),
    ...(input.models ? { models: input.models } : {}),
    ...(input.trust ? { trust: input.trust } : {}),
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

export function validateAciCloudConfig(
  raw: unknown,
  configPath = "<aci-config>",
  providerProfile: ProviderProfile = DEFAULT_PROFILE,
): AciCloudConfig {
  const config = requireRecord(raw, configPath, "");
  const models = requireRecord(config.models, configPath, "/models");
  const trust = requireRecord(config.trust, configPath, "/trust");
  for (const [record, field, pointer] of [
    [config, "baseUrl", "/baseUrl"],
    [models, "isTeeOnly", "/models/isTeeOnly"],
    [models, "thinkingFormat", "/models/thinkingFormat"],
  ] as const) {
    if (!(field in record)) fail(configPath, pointer, "required field is missing");
  }
  try {
    const resolved = resolveAciProviderConfig(
      providerProfile,
      {
        baseURL: config.baseUrl,
        models: {
          isTeeOnly: models.isTeeOnly,
          thinkingFormat: models.thinkingFormat,
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
    return {
      baseUrl: resolved.baseURL,
      models: {
        isTeeOnly: resolved.models.isTeeOnly,
        thinkingFormat: resolved.models.thinkingFormat,
        ...(resolved.models.allowlist ? { allowlist: [...resolved.models.allowlist] } : {}),
      },
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
      readConfigFileQuiet(
        getProjectAciCloudConfigPath(cwd, providerProfile.providerId),
        providerProfile.logPrefix,
      ),
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
      readConfigFileQuiet(
        getGlobalAciCloudConfigPath(home, providerProfile.providerId),
        providerProfile.logPrefix,
      ),
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
