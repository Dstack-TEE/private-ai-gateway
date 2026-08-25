// ACI attestation client.
//
// Thin provider-side wrapper over the repo's reference verifier
// `@phala/aci-verifier` (clients/verifier-ts), used ONLY for the prevention
// layer: validating the gateway's attestation report so we can pin its TLS
// SPKI. Per-response receipt verification is deliberately NOT wired here —
// pinning is the security control (traffic is readable only by the attested
// workload); receipts would be a post-hoc audit, which this plugin does not
// ship.
//
// The verifier first checks the report binding, then verifies the TDX quote to
// the Intel root and confirms that the quote binds the same report_data. The
// SPKI pin therefore comes from a genuine TEE report, not merely a
// self-consistent document supplied by the endpoint.

import { randomBytes } from "node:crypto";

import {
  type AttestationReport,
  type TlsKeyPin,
  type WorkloadKeyset,
  AciError,
  verifyQuote,
  verifyReportBinding,
} from "@phala/aci-verifier";

import {
  DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
  LOG_PREFIX,
  buildAttestationUrl,
  buildReceiptUrl,
  buildSessionUrl,
} from "./constants.ts";

// ----------------------------------------------------------------------------
// Fetch layer (returns null on failure; never throws for the caller)
// ----------------------------------------------------------------------------

async function fetchJson(
  url: string,
  apiKey: string,
  timeoutMs: number,
  label: string,
): Promise<unknown | null> {
  if (!apiKey) return null;
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), timeoutMs).unref();
  try {
    const response = await fetch(url, {
      signal: controller.signal,
      headers: { Authorization: `Bearer ${apiKey}`, Accept: "application/json" },
    });
    if (!response.ok) {
      console.error(`${LOG_PREFIX} ${label} returned ${response.status} ${response.statusText}`);
      return null;
    }
    const text = await response.text();
    try {
      return JSON.parse(text);
    } catch (parseError) {
      console.error(`${LOG_PREFIX} ${label} JSON parse failed:`, parseError);
      return null;
    }
  } catch (error) {
    console.error(`${LOG_PREFIX} ${label} failed:`, error);
    return null;
  } finally {
    clearTimeout(timeout);
  }
}

export async function fetchAttestation(
  apiKey: string,
  nonce: string,
  options: { timeoutMs?: number; baseUrl?: string } = {},
): Promise<AttestationReport | null> {
  const json = await fetchJson(
    buildAttestationUrl(nonce, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
    "attestation",
  );
  return json as AttestationReport | null;
}

// ----------------------------------------------------------------------------
// On-request audit fetches (raw documents — no verification). These power the
// /aci-receipt and /aci-session commands. They are NOT wired into the chat
// path: prevention is pinning, and receipts/sessions are only shown when the
// user asks.
// ----------------------------------------------------------------------------

/** Fetch a receipt document by id (raw; no signature verification). */
export async function fetchReceipt(
  apiKey: string,
  receiptId: string,
  options: { timeoutMs?: number; baseUrl?: string } = {},
): Promise<Record<string, unknown> | null> {
  return (await fetchJson(
    buildReceiptUrl(receiptId, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
    `receipt ${receiptId}`,
  )) as Record<string, unknown> | null;
}

/** Fetch an attested session document by id (raw). */
export async function fetchSession(
  apiKey: string,
  sessionId: string,
  options: { timeoutMs?: number; baseUrl?: string } = {},
): Promise<Record<string, unknown> | null> {
  return (await fetchJson(
    buildSessionUrl(sessionId, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
    `session ${sessionId}`,
  )) as Record<string, unknown> | null;
}

/** Compact summary of a raw receipt's key fields for the on-request audit
 *  display. Always present: receipt_id, key_id, api_version, served_at, model,
 *  and a one-line rendering of each event_log entry (type + selected fields). */
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
  push("Served at", receipt.served_at ? new Date(Number(receipt.served_at) * 1000).toISOString() : receipt.served_at);
  push("Keyset digest", receipt.workload_keyset_digest);
  const events = Array.isArray(receipt.event_log) ? (receipt.event_log as unknown[]) : [];
  lines.push(`events: ${events.length}`);
  const interesting = ["upstream.verified", "request.received", "request.forwarded", "response.received", "response.returned"];
  for (const ev of events) {
    const e = ev as Record<string, unknown>;
    const type = String(e.type ?? "?");
    if (!interesting.includes(type)) continue;
    const summary = summarizeEvent(e, type);
    if (summary) lines.push(`  ${type} ${summary}`);
  }
  return lines;
}

function summarizeEvent(e: Record<string, unknown>, type: string): string {
  if (type === "upstream.verified") {
    const bits: string[] = [];
    if (e.result !== undefined) bits.push(`result=${String(e.result)}`);
    if (e.required !== undefined) bits.push(`required=${String(e.required)}`);
    if (e.provider !== undefined) bits.push(`provider=${String(e.provider)}`);
    if (e.model_id !== undefined) bits.push(`model=${String(e.model_id)}`);
    if (e.session_id !== undefined) bits.push(`session=${String(e.session_id)}`);
    return bits.length ? bits.join(" ") : "";
  }
  if (e.body_hash !== undefined) return `body_hash=${String(e.body_hash)}`;
  return "";
}

/** Compact summary of a raw attested session's key fields. */
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

// ----------------------------------------------------------------------------
// Attestation verification (verifier-ts §9.1 checks 1–3)
// ----------------------------------------------------------------------------

/** Verify an attestation report and bind it to the nonce we sent. Returns null
 *  when the binding or hardware quote fails, or when the report is malformed.
 *  A bad caller nonce can still throw; callers always generate it locally. */
export async function bindAttestation(
  report: AttestationReport,
  nonce: string,
): Promise<import("@phala/aci-verifier").ReportVerification | null> {
  try {
    const verification = await verifyReportBinding(report, nonce);
    if (!verification.ok) {
      console.error(
        `${LOG_PREFIX} attestation binding failed: ${verification.checks
          .filter((c) => !c.ok)
          .map((c) => `${c.name}: ${c.detail ?? "fail"}`)
          .join("; ")}`,
      );
      return null;
    }
    const quote = await verifyQuote(report);
    if (!quote.ok) {
      console.error(
        `${LOG_PREFIX} attestation quote verification failed: ${quote.detail ?? "unknown failure"}`,
      );
      return null;
    }
    return verification;
  } catch (error) {
    if (error instanceof AciError) {
      console.error(`${LOG_PREFIX} attestation binding rejected:`, error.message);
      return null;
    }
    throw error;
  }
}

/** Server-freshness cut-off (Unix ms) from an established keyset, if the keyset
 *  carries a numeric `not_after` (§9.1 check 3). Absent → undefined (caller
 *  falls back to its own TTL). */
export function keysetStaleAfterMs(keyset: WorkloadKeyset | undefined): number | undefined {
  const notAfter = keyset?.not_after;
  return typeof notAfter === "number" && Number.isFinite(notAfter) ? notAfter * 1000 : undefined;
}

// ----------------------------------------------------------------------------
// TLS SPKI extraction for pinning (§3.1 tls_public_keys)
// ----------------------------------------------------------------------------

/** First TLS SPKI SHA-256 that matches the host (either unscoped or by its
 *  `domain`), from an established keyset. Returns undefined when the keyset
 *  carries no TLS keys for that host. */
export function attestedSpkiSha256ForHost(
  keyset: WorkloadKeyset | undefined,
  host: string,
): string | undefined {
  if (!keyset || !Array.isArray(keyset.tls_public_keys)) return undefined;
  const normalized = host.trim().toLowerCase();
  const scoped = (keyset.tls_public_keys as TlsKeyPin[]).find(
    (pin) => pin.domain !== undefined && pin.domain.toLowerCase() === normalized,
  );
  const entry = scoped ?? (keyset.tls_public_keys as TlsKeyPin[]).find((pin) => pin.domain === undefined);
  if (!entry || typeof entry.spki_sha256 !== "string" || entry.spki_sha256.length === 0) {
    return undefined;
  }
  return entry.spki_sha256.toLowerCase();
}

/** Re-export the receipt signing keys of a keyset (for the /attestation
 *  command's informational display). */
export function receiptSigningKeys(keyset: WorkloadKeyset | undefined): Array<{
  key_id: string;
  algo: string;
}> {
  if (!Array.isArray(keyset?.receipt_signing_keys)) return [];
  return (keyset.receipt_signing_keys as Array<{ key_id?: unknown; algo?: unknown }>)
    .filter((k) => typeof k.key_id === "string" && typeof k.algo === "string")
    .map((k) => ({ key_id: k.key_id as string, algo: k.algo as string }));
}

/** Generate a fresh nonce (32 random bytes, 64 lowercase hex — §3.2). */
export function newNonce(): string {
  return randomBytes(32).toString("hex");
}
