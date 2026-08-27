import {
  DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
  DEFAULT_RECEIPT_FETCH_TIMEOUT_MS,
  LOG_PREFIX,
  buildReceiptUrl,
  buildSessionUrl,
} from "./constants.ts";

interface AuditFetchOptions {
  timeoutMs?: number;
  baseUrl?: string;
  fetch?: typeof globalThis.fetch;
  logPrefix?: string;
}

async function fetchJson(
  url: string,
  apiKey: string,
  timeoutMs: number,
  label: string,
  fetchImpl: typeof globalThis.fetch,
  logPrefix: string,
): Promise<Record<string, unknown> | null> {
  if (!apiKey) return null;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs).unref();
  try {
    const response = await fetchImpl(url, {
      signal: controller.signal,
      headers: { Authorization: `Bearer ${apiKey}`, Accept: "application/json" },
    });
    if (!response.ok) {
      console.error(`${logPrefix} ${label} returned ${response.status} ${response.statusText}`);
      return null;
    }
    const value = (await response.json()) as unknown;
    return typeof value === "object" && value !== null && !Array.isArray(value)
      ? (value as Record<string, unknown>)
      : null;
  } catch (error) {
    console.error(`${logPrefix} ${label} failed:`, error);
    return null;
  } finally {
    clearTimeout(timeout);
  }
}

export function fetchReceipt(
  apiKey: string,
  receiptId: string,
  options: AuditFetchOptions = {},
): Promise<Record<string, unknown> | null> {
  return fetchJson(
    buildReceiptUrl(receiptId, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_RECEIPT_FETCH_TIMEOUT_MS,
    `receipt ${receiptId}`,
    options.fetch ?? globalThis.fetch,
    options.logPrefix ?? LOG_PREFIX,
  );
}

export function fetchSession(
  apiKey: string,
  sessionId: string,
  options: AuditFetchOptions = {},
): Promise<Record<string, unknown> | null> {
  return fetchJson(
    buildSessionUrl(sessionId, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
    `session ${sessionId}`,
    options.fetch ?? globalThis.fetch,
    options.logPrefix ?? LOG_PREFIX,
  );
}

export function summarizeReceipt(receipt: Record<string, unknown>): string[] {
  const lines: string[] = [];
  const push = (label: string, value: unknown): void => {
    lines.push(`${label}: ${value === undefined || value === null ? "(none)" : String(value)}`);
  };
  push("Receipt", receipt.receipt_id);
  push("API version", receipt.api_version);
  push("Signing key", receipt.key_id);
  push("Model", receipt.model);
  push("Endpoint", receipt.endpoint);
  push(
    "Served at",
    receipt.served_at
      ? new Date(Number(receipt.served_at) * 1000).toISOString()
      : receipt.served_at,
  );
  push("Keyset digest", receipt.workload_keyset_digest);
  const events = Array.isArray(receipt.event_log) ? receipt.event_log : [];
  lines.push(`events: ${events.length}`);
  const interesting = new Set([
    "upstream.verified",
    "request.received",
    "request.forwarded",
    "response.received",
    "response.returned",
  ]);
  for (const event of events) {
    if (typeof event !== "object" || event === null || Array.isArray(event)) continue;
    const type = String(event.type ?? "?");
    if (!interesting.has(type)) continue;
    const summary = summarizeEvent(event, type);
    if (summary) lines.push(`  ${type} ${summary}`);
  }
  return lines;
}

function summarizeEvent(event: Record<string, unknown>, type: string): string {
  if (type === "upstream.verified") {
    const fields: string[] = [];
    for (const key of ["result", "required", "provider", "model_id", "session_id"]) {
      if (event[key] !== undefined) fields.push(`${key}=${String(event[key])}`);
    }
    return fields.join(" ");
  }
  return event.body_hash === undefined ? "" : `body_hash=${String(event.body_hash)}`;
}

export function summarizeSession(session: Record<string, unknown>): string[] {
  return [
    `Session: ${String(session.session_id ?? session.id ?? "?")}`,
    `API version: ${String(session.api_version ?? "?")}`,
    `Upstream: ${String(session.upstream_name ?? "?")}`,
    `Endpoint: ${String(session.endpoint ?? "(none)")}`,
    `Verifier: ${String(session.verifier_id ?? "?")}`,
    `Established: ${session.established_at ? new Date(Number(session.established_at) * 1000).toISOString() : "?"}`,
    `Expires: ${session.expires_at ? new Date(Number(session.expires_at) * 1000).toISOString() : "?"}`,
  ];
}
