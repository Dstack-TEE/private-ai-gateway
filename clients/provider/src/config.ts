import type { AciProviderProfile } from "./profile.ts";

export type AciReceiptVerification = "on-demand" | "response";

export interface AciProviderConfig {
  baseURL: string;
  models: {
    isTeeOnly: boolean;
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

function envValue(env: Record<string, string | undefined>, ...names: string[]): string | undefined {
  for (const name of names) {
    const value = env[name]?.trim();
    if (value) return value;
  }
  return undefined;
}

function commaSeparated(value: string | undefined): string[] | undefined {
  return value?.split(",").map((item) => item.trim());
}

function booleanEnv(value: string | undefined): boolean | string | undefined {
  if (value === undefined) return undefined;
  const normalized = value.toLowerCase();
  if (normalized === "1" || normalized === "true") return true;
  if (normalized === "0" || normalized === "false") return false;
  return value;
}

/** Read the canonical ACI environment variables for any host adapter. */
export function aciProviderConfigInputFromEnv(
  profile: AciProviderProfile,
  env: Record<string, string | undefined> = process.env,
): AciProviderConfigInput {
  const prefix = profile.envPrefix;
  const baseURL = envValue(env, `${prefix}_BASE_URL`);
  const isTeeOnly = booleanEnv(envValue(env, `${prefix}_IS_TEE_ONLY`));
  const allowlist = commaSeparated(envValue(env, `${prefix}_MODEL_ALLOWLIST`));
  const acceptedComposeHashes = commaSeparated(envValue(env, `${prefix}_ACCEPTED_COMPOSE_HASHES`));
  const acceptedSessionIds = commaSeparated(envValue(env, `${prefix}_ACCEPTED_SESSION_IDS`));

  return {
    ...(baseURL ? { baseURL } : {}),
    ...(isTeeOnly !== undefined || allowlist !== undefined
      ? {
          models: {
            ...(isTeeOnly !== undefined ? { isTeeOnly } : {}),
            ...(allowlist !== undefined ? { allowlist } : {}),
          },
        }
      : {}),
    ...(acceptedComposeHashes !== undefined || acceptedSessionIds !== undefined
      ? {
          trust: {
            ...(acceptedComposeHashes !== undefined ? { acceptedComposeHashes } : {}),
            ...(acceptedSessionIds !== undefined ? { acceptedSessionIds } : {}),
          },
        }
      : {}),
  };
}

export function resolveAciProviderConfig(
  profile: AciProviderProfile,
  input: AciProviderConfigInput = {},
  env: Record<string, string | undefined> = process.env,
): AciProviderConfig {
  const envInput = aciProviderConfigInputFromEnv(profile, env);
  const rawBaseURL =
    input.baseURL === undefined ? (envInput.baseURL ?? profile.defaultBaseURL) : input.baseURL;
  if (typeof rawBaseURL !== "string" || rawBaseURL.trim().length === 0) {
    fail("/baseURL", "expected a non-empty URL");
  }
  const baseURL = rawBaseURL.trim();
  try {
    const parsed = new URL(baseURL);
    if (parsed.protocol !== "https:") fail("/baseURL", "expected an https URL");
  } catch (error) {
    if (error instanceof AciProviderConfigError) throw error;
    fail("/baseURL", "expected a valid URL");
  }

  const isTeeOnly =
    input.models?.isTeeOnly === undefined
      ? (envInput.models?.isTeeOnly ?? true)
      : input.models.isTeeOnly;
  if (typeof isTeeOnly !== "boolean") fail("/models/isTeeOnly", "expected a boolean");

  const allowlist = optionalStringArray(
    input.models?.allowlist === undefined ? envInput.models?.allowlist : input.models.allowlist,
    "/models/allowlist",
  );
  const acceptedComposeHashes = validateComposeHashes(
    input.trust?.acceptedComposeHashes === undefined
      ? (envInput.trust?.acceptedComposeHashes ?? profile.acceptedComposeHashes)
      : input.trust.acceptedComposeHashes,
  );
  const acceptedSessionIds = validateSessionIds(
    input.trust?.acceptedSessionIds === undefined
      ? (envInput.trust?.acceptedSessionIds ?? profile.acceptedSessionIds)
      : input.trust.acceptedSessionIds,
  );

  const verification =
    input.receipts?.verification === undefined ? "on-demand" : input.receipts.verification;
  if (verification !== "on-demand" && verification !== "response") {
    fail("/receipts/verification", 'expected "on-demand" or "response"');
  }
  const historySize = input.receipts?.historySize === undefined ? 32 : input.receipts.historySize;
  if (
    typeof historySize !== "number" ||
    !Number.isInteger(historySize) ||
    historySize < 1 ||
    historySize > 1000
  ) {
    fail("/receipts/historySize", "expected an integer between 1 and 1000");
  }

  return {
    baseURL,
    models: {
      isTeeOnly,
      ...(allowlist ? { allowlist } : {}),
    },
    trust: {
      ...(acceptedComposeHashes ? { acceptedComposeHashes } : {}),
      ...(acceptedSessionIds ? { acceptedSessionIds } : {}),
    },
    receipts: { verification, historySize },
  };
}
