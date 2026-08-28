import type { AciFetch } from "@phala/aci-verifier/runtime";

import type {
  AccountApiKeyAuth,
  AccountApiKeyCredential,
  CompleteAccountApiKeyAuthorizationOptions,
} from "./account-auth.ts";
import { phalaCloudEndpoint, startPhalaCloudDeviceAuthorization } from "./device-auth.ts";

export {
  startPhalaCloudDeviceAuthorization,
  type PhalaCloudApiKey,
  type PhalaCloudDeviceAuthorization,
  type PhalaCloudDeviceAuthorizationOptions,
  type PhalaCloudDeviceAuthorizationPollOptions,
} from "./device-auth.ts";

export const DEFAULT_PHALA_CLOUD_API_BASE_URL = "https://cloud-api.phala.com";

export function resolvePhalaCloudApiBaseURL(
  env: Record<string, string | undefined> = process.env,
): string {
  return (
    env.PHALA_CLOUD_API_BASE_URL?.trim().replace(/\/+$/, "") || DEFAULT_PHALA_CLOUD_API_BASE_URL
  );
}

export interface PhalaCloudAccount {
  username?: string;
  workspaceName?: string;
  workspaceSlug?: string;
}

export interface PhalaCloudAccountAuthorizationOptions {
  baseURL: string;
  clientId: string;
  fetch?: AciFetch;
  signal?: AbortSignal;
  accountMetadataTimeoutMs?: number;
}

export interface CompletePhalaCloudAccountAuthorizationOptions extends CompleteAccountApiKeyAuthorizationOptions {
  includeAccountMetadata?: boolean;
}

export type PhalaCloudAccountApiKey = AccountApiKeyCredential;

export interface PhalaCloudAccountAuthorization {
  userCode: string;
  verificationURI: string;
  expiresIn: number;
  interval: number;
  complete(
    options?: CompletePhalaCloudAccountAuthorizationOptions,
  ): Promise<PhalaCloudAccountApiKey>;
}

export interface CreatePhalaCloudAccountAuthOptions {
  label?: string;
  baseURL: string;
  clientId: string;
  fetch?: AciFetch;
  includeAccountMetadata?: boolean;
  accountMetadataTimeoutMs?: number;
}

export interface FetchPhalaCloudAccountOptions {
  baseURL: string;
  apiKey: string;
  fetch?: AciFetch;
  signal?: AbortSignal;
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

export async function fetchPhalaCloudAccount({
  baseURL,
  apiKey,
  fetch = globalThis.fetch,
  signal,
}: FetchPhalaCloudAccountOptions): Promise<PhalaCloudAccount> {
  const response = await fetch(phalaCloudEndpoint(baseURL, "/api/v1/private_ai/self"), {
    headers: { Authorization: `Bearer ${apiKey}`, Accept: "application/json" },
    signal,
  });
  if (!response.ok) {
    throw new Error(`Phala Cloud account request failed with HTTP ${response.status}`);
  }
  const data = record(await response.json());
  if (!data) throw new Error("Phala Cloud account endpoint returned an invalid response");
  const user = record(data.user);
  const workspace = record(data.workspace);
  return {
    ...(typeof user?.username === "string" ? { username: user.username } : {}),
    ...(typeof workspace?.name === "string" ? { workspaceName: workspace.name } : {}),
    ...(typeof workspace?.slug === "string" ? { workspaceSlug: workspace.slug } : {}),
  };
}

export async function startPhalaCloudAccountAuthorization({
  baseURL,
  clientId,
  fetch,
  signal,
  accountMetadataTimeoutMs = 5_000,
}: PhalaCloudAccountAuthorizationOptions): Promise<PhalaCloudAccountAuthorization> {
  if (!Number.isFinite(accountMetadataTimeoutMs) || accountMetadataTimeoutMs <= 0) {
    throw new Error("accountMetadataTimeoutMs must be a positive number");
  }
  const authorization = await startPhalaCloudDeviceAuthorization({
    baseURL,
    clientId,
    ...(fetch ? { fetch } : {}),
    ...(signal ? { signal } : {}),
  });

  return {
    userCode: authorization.userCode,
    verificationURI: authorization.verificationURI,
    expiresIn: authorization.expiresIn,
    interval: authorization.interval,
    async complete(options = {}) {
      const completionSignal = options.signal ?? signal;
      const token = await authorization.poll({
        ...(completionSignal ? { signal: completionSignal } : {}),
        ...(options.onProgress ? { onProgress: options.onProgress } : {}),
      });
      const metadata: Record<string, string> = {};
      if (token.keyId !== undefined) metadata.keyId = String(token.keyId);

      if (options.includeAccountMetadata !== false) {
        try {
          const timeoutSignal = AbortSignal.timeout(accountMetadataTimeoutMs);
          const account = await fetchPhalaCloudAccount({
            baseURL,
            apiKey: token.accessToken,
            ...(fetch ? { fetch } : {}),
            signal: completionSignal
              ? AbortSignal.any([completionSignal, timeoutSignal])
              : timeoutSignal,
          });
          if (account.username) metadata.username = account.username;
          if (account.workspaceName) metadata.workspaceName = account.workspaceName;
          if (account.workspaceSlug) metadata.workspaceSlug = account.workspaceSlug;
        } catch (error) {
          if (completionSignal?.aborted) {
            throw new Error("Device authorization cancelled", { cause: error });
          }
          // Account metadata is optional; the issued inference key remains valid.
        }
      }

      return {
        apiKey: token.accessToken,
        ...(Object.keys(metadata).length > 0 ? { metadata } : {}),
      };
    },
  };
}

export function createPhalaCloudAccountAuth({
  label = "Phala Cloud account",
  baseURL,
  clientId,
  fetch,
  includeAccountMetadata = true,
  accountMetadataTimeoutMs,
}: CreatePhalaCloudAccountAuthOptions): AccountApiKeyAuth {
  return {
    label,
    async start(options = {}) {
      const authorization = await startPhalaCloudAccountAuthorization({
        baseURL,
        clientId,
        ...(fetch ? { fetch } : {}),
        ...(options.signal ? { signal: options.signal } : {}),
        ...(accountMetadataTimeoutMs === undefined ? {} : { accountMetadataTimeoutMs }),
      });
      return {
        url: authorization.verificationURI,
        instructions: `Approve the device login with code ${authorization.userCode}`,
        presentation: {
          type: "device_code",
          userCode: authorization.userCode,
          intervalSeconds: authorization.interval,
          expiresInSeconds: authorization.expiresIn,
        },
        async complete(completion = {}) {
          return authorization.complete({
            ...(completion.signal ? { signal: completion.signal } : {}),
            ...(completion.onProgress ? { onProgress: completion.onProgress } : {}),
            includeAccountMetadata,
          });
        },
      };
    },
  };
}
