import type { AciFetch } from "@phala/aci-verifier/runtime";

import { phalaCloudEndpoint } from "./device-auth.ts";

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
