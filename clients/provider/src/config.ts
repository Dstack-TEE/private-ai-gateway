import type { AciProviderProfile } from "./profile.ts";

export type AciThinkingFormat = "auto" | "qwen" | "openai" | "off";
export type AciReceiptVerification = "on-demand" | "response";

export interface AciProviderConfig {
  baseURL: string;
  models: {
    isTeeOnly: boolean;
    thinkingFormat: AciThinkingFormat;
    allowlist?: readonly string[];
  };
  trust: {
    acceptedComposeHashes?: readonly string[];
    acceptedSessionIds?: readonly string[];
  };
  receipts: {
    verification: AciReceiptVerification;
    historySize: number;
  };
}

export interface AciProviderConfigInput {
  baseURL?: unknown;
  models?: {
    isTeeOnly?: unknown;
    thinkingFormat?: unknown;
    allowlist?: unknown;
  };
  trust?: {
    acceptedComposeHashes?: unknown;
    acceptedSessionIds?: unknown;
  };
  receipts?: {
    verification?: unknown;
    historySize?: unknown;
  };
}

export class AciProviderConfigError extends Error {
  public readonly pointer: string;

  constructor(message: string, pointer: string) {
    super(`${pointer}: ${message}`);
    this.name = "AciProviderConfigError";
    this.pointer = pointer;
  }
}

function fail(pointer: string, message: string): never {
  throw new AciProviderConfigError(message, pointer);
}

function optionalStringArray(value: unknown, pointer: string): readonly string[] | undefined {
  if (value === undefined) return undefined;
  if (!Array.isArray(value) || value.length === 0) {
    return fail(pointer, "expected a non-empty string array");
  }
  return value.map((item, index) => {
    if (typeof item !== "string" || item.length === 0) {
      return fail(`${pointer}/${index}`, "expected a non-empty string");
    }
    return item;
  });
}

function validateComposeHashes(value: unknown): readonly string[] | undefined {
  return optionalStringArray(value, "/trust/acceptedComposeHashes")?.map((hash, index) => {
    if (!/^[0-9a-f]{64}$/i.test(hash)) {
      return fail(
        `/trust/acceptedComposeHashes/${index}`,
        "expected a 64-character SHA-256 digest",
      );
    }
    return hash.toLowerCase();
  });
}

function validateSessionIds(value: unknown): readonly string[] | undefined {
  return optionalStringArray(value, "/trust/acceptedSessionIds")?.map((id, index) => {
    if (!/^[0-9a-f]{64}$/.test(id)) {
      return fail(
        `/trust/acceptedSessionIds/${index}`,
        "expected a 64-character lowercase session id",
      );
    }
    return id;
  });
}

export function resolveAciProviderConfig(
  profile: AciProviderProfile,
  input: AciProviderConfigInput = {},
  env: Record<string, string | undefined> = process.env,
): AciProviderConfig {
  const envValue = (...names: string[]) => {
    for (const name of names) {
      const value = env[name]?.trim();
      if (value) return value;
    }
    return undefined;
  };
  const prefix = profile.envPrefix;
  const baseURL =
    (typeof input.baseURL === "string" ? input.baseURL.trim() : "") ||
    envValue(
      `${prefix}_BASE_URL`,
      `${prefix}_CLOUD_API_PREFIX`,
      ...(profile.baseURLAliases ?? []),
    ) ||
    profile.defaultBaseURL;
  if (!baseURL) fail("/baseURL", "expected a non-empty URL");
  try {
    const parsed = new URL(baseURL);
    if (parsed.protocol !== "https:") fail("/baseURL", "expected an https URL");
  } catch (error) {
    if (error instanceof AciProviderConfigError) throw error;
    fail("/baseURL", "expected a valid URL");
  }

  const teeEnv = envValue(`${prefix}_IS_TEE_ONLY`, `${prefix}_TEE_ONLY`);
  const isTeeOnly =
    input.models?.isTeeOnly ??
    (teeEnv === undefined ? true : teeEnv === "1" || teeEnv.toLowerCase() === "true");
  if (typeof isTeeOnly !== "boolean") fail("/models/isTeeOnly", "expected a boolean");

  const thinkingFormat =
    input.models?.thinkingFormat ?? envValue(`${prefix}_THINKING_FORMAT`) ?? "auto";
  if (!(["auto", "qwen", "openai", "off"] as const).includes(thinkingFormat as never)) {
    fail("/models/thinkingFormat", 'expected "auto", "qwen", "openai", or "off"');
  }

  const allowlist = optionalStringArray(
    input.models?.allowlist ??
      envValue(`${prefix}_MODEL_ALLOWLIST`)
        ?.split(",")
        .map((v) => v.trim()),
    "/models/allowlist",
  );
  const acceptedComposeHashes =
    validateComposeHashes(
      input.trust?.acceptedComposeHashes ??
        envValue(`${prefix}_ACCEPTED_COMPOSE_HASHES`)
          ?.split(",")
          .map((v) => v.trim()),
    ) ?? profile.acceptedComposeHashes;
  const acceptedSessionIds =
    validateSessionIds(
      input.trust?.acceptedSessionIds ??
        envValue(`${prefix}_ACCEPTED_SESSION_IDS`)
          ?.split(",")
          .map((v) => v.trim()),
    ) ?? profile.acceptedSessionIds;

  const verification = input.receipts?.verification ?? "on-demand";
  if (verification !== "on-demand" && verification !== "response") {
    fail("/receipts/verification", 'expected "on-demand" or "response"');
  }
  const historySize = input.receipts?.historySize ?? 32;
  if (!Number.isInteger(historySize) || Number(historySize) < 1 || Number(historySize) > 1000) {
    fail("/receipts/historySize", "expected an integer between 1 and 1000");
  }

  return {
    baseURL,
    models: {
      isTeeOnly,
      thinkingFormat: thinkingFormat as AciThinkingFormat,
      ...(allowlist ? { allowlist } : {}),
    },
    trust: {
      ...(acceptedComposeHashes ? { acceptedComposeHashes } : {}),
      ...(acceptedSessionIds ? { acceptedSessionIds } : {}),
    },
    receipts: { verification, historySize: Number(historySize) },
  };
}
