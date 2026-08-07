// ACI verification client.
//
// This is a thin provider-side wrapper over the repo's reference verifier,
// `@phala/aci-verifier` (clients/verifier-ts), which implements the ACI spec
// this repo defines and ships. The pi provider deliberately does NOT reimplement
// report binding, receipt signature verification, or body-hash checks — the
// reference verifier is the single source of truth, verified against
// spec/test-vectors.md. This module only:
//   - fetches the ACI artifacts (attestation, receipt, session) over HTTP;
//   - maps verifier-ts results onto the provider's footer classification
//     (verified / verified* / routed / attested / mismatch);
//   - extracts the attested TLS SPKI for pinning from the established keyset.
//
// Fail-closed philosophy: a `verified` footer requires the binding, the
// signature, AND the body hashes to be checked and pass. When bytes are not
// available (pi does not hand the extension the raw SSE bytes) we say so
// explicitly in the status rather than silently claiming byte-binding.

import { randomBytes } from "node:crypto";

import {
  type AttestationReport,
  type KeysetKey,
  type ReceiptEnvelope,
  type ReceiptPayload,
  type ReportVerification,
  type TlsKeyPin,
  type WorkloadKeyset,
  AciError,
  checkRequestBodyHash,
  checkResponseBodyHash,
  findEvent,
  verifyReceipt as verifyReceiptReference,
  verifyReportBinding,
} from "@phala/aci-verifier";

import {
  DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
  DEFAULT_RECEIPT_FETCH_TIMEOUT_MS,
  LOG_PREFIX,
  buildAttestationUrl,
  buildReceiptUrl,
  buildSessionUrl,
} from "./constants.ts";

// ----------------------------------------------------------------------------
// Fetch layer (identical to the previous verify.ts; returns null on failure)
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

export async function fetchReceipt(
  apiKey: string,
  receiptId: string,
  options: { timeoutMs?: number; baseUrl?: string } = {},
): Promise<ReceiptEnvelope | null> {
  const json = await fetchJson(
    buildReceiptUrl(receiptId, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_RECEIPT_FETCH_TIMEOUT_MS,
    `receipt ${receiptId}`,
  );
  return json as ReceiptEnvelope | null;
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

export async function fetchSession(
  apiKey: string,
  sessionId: string,
  options: { timeoutMs?: number; baseUrl?: string } = {},
): Promise<unknown | null> {
  const json = await fetchJson(
    buildSessionUrl(sessionId, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_RECEIPT_FETCH_TIMEOUT_MS,
    `session ${sessionId}`,
  );
  return json ?? null;
}

// ----------------------------------------------------------------------------
// Binding (verifier-ts §9.1 checks 2–3)
// ----------------------------------------------------------------------------

/** Validate an attestation report against the nonce we sent. Returns null when
 *  the binding fails or the report is malformed — never throws for a failed
 *  check (only for a bad caller nonce, which we always generate). */
export async function bindAttestation(
  report: AttestationReport,
  nonce: string,
): Promise<ReportVerification | null> {
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

// ----------------------------------------------------------------------------
// Receipt verification + footer classification
// ----------------------------------------------------------------------------

export type ReceiptStatus = "verified" | "routed" | "unknown";

export interface ReceiptClassification {
  status: ReceiptStatus;
  provider?: string;
  modelId?: string;
  sessionId?: string;
  required?: boolean;
  /** Receipt signature + version + keyset binding all passed (§9.3 checks 1–2). */
  signatureValid?: boolean;
  /** request.received.body_hash matched our sent bytes (checked only when provided). */
  requestHashValid?: boolean;
  /** response.returned.body_hash matched the bytes we saw (checked only when provided). */
  responseHashValid?: boolean;
  /** True when request/response hashes were actually checked (bytes were provided). */
  hashesChecked?: boolean;
  /** When hashes were not checked, why (pi does not expose the raw bytes). */
  hashesNotCheckedReason?: string;
  /** upstream.verified classification (aggregator receipts). */
  upstreamVerified?: boolean;
  witnessSkipped?: boolean;
  sessionVerified?: boolean;
}

function classifyUpstream(payload: ReceiptPayload | undefined): {
  status: ReceiptStatus;
  provider?: string;
  modelId?: string;
  sessionId?: string;
  required?: boolean;
} {
  if (!payload) return { status: "unknown" };
  const event = findEvent(payload, "upstream.verified");
  const base: { status: ReceiptStatus; provider?: string; modelId?: string; sessionId?: string; required?: boolean } = {
    status: "unknown",
  };
  if (!event) return base;
  const result = event.result;
  const required = event.required === true;
  const provider = typeof event.provider === "string" ? event.provider : undefined;
  const modelId = typeof event.model_id === "string" ? event.model_id : undefined;
  const sessionId = typeof event.session_id === "string" ? event.session_id : undefined;
  base.provider = provider;
  base.modelId = modelId;
  base.sessionId = sessionId;
  base.required = required;
  if (result === "verified" && required) base.status = "verified";
  else if (result === "failed" && !required) base.status = "routed";
  else base.status = "unknown";
  return base;
}

/**
 * Verify a receipt against an established (bound) keyset and classify it for
 * the footer. `establishedDigest` is the digest the binding established. When
 * `requestBody`/`responseBytes` are provided the body-hash checks run; when
 * absent they are reported as un-checked (never silently passed).
 */
export async function classifyReceipt(
  receipt: ReceiptEnvelope,
  keyset: WorkloadKeyset,
  establishedDigest: string,
  options: { requestBody?: Uint8Array; responseBytes?: Uint8Array } = {},
): Promise<ReceiptClassification> {
  // §9.3 checks 1–2: signature over JCS(minus signature) + version + digest binding.
  const verification = await verifyReceiptReference(receipt, keyset, establishedDigest);
  const signatureValid = verification.ok;

  const payload = verification.payload;
  const upstream = classifyUpstream(payload);

  const classification: ReceiptClassification = {
    ...upstream,
    signatureValid,
  };

  if (payload) {
    classification.modelId = typeof payload.model === "string" ? payload.model : undefined;
  }

  // Body hashes (§9.3 checks 3–4). Only claim a check when we have the bytes.
  if (options.requestBody) {
    classification.requestHashValid = payload
      ? await checkRequestBodyHash(payload, options.requestBody)
      : false;
  }
  if (options.responseBytes) {
    classification.responseHashValid = payload
      ? await checkResponseBodyHash(payload, options.responseBytes)
      : false;
  }
  const hashesChecked = options.requestBody !== undefined || options.responseBytes !== undefined;
  classification.hashesChecked = hashesChecked;
  if (!hashesChecked && payload) {
    // pi only hands the extension response headers, not the raw SSE stream, so
    // the wire bytes the receipt's response.returned.body_hash commits to are
    // not available inside the extension. State it, don't hide it.
    classification.hashesNotCheckedReason =
      "pi does not expose the raw response stream bytes to extensions; body hashes not checked here";
  }
  return classification;
}

/** Overall verified for the footer: signature passed AND the body hashes were
 *  checked and passed. Missing, un-checked hashes never count as a pass. */
export function isFullyVerified(classification: ReceiptClassification): boolean {
  if (classification.signatureValid !== true) return false;
  if (classification.requestHashValid === false || classification.responseHashValid === false) {
    return false;
  }
  return classification.hashesChecked === true;
}

/** Extract a raw request body payload to later compare against
 *  request.received.body_hash. Serializes with canonical (sorted) member
 *  order so the bytes are stable regardless of insertion order. */
export function canonicalRequestBytes(payload: unknown): Uint8Array {
  return new TextEncoder().encode(canonicalizeJson(payload));
}

function canonicalizeJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalizeJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const obj = value as Record<string, unknown>;
    const out: string[] = [];
    for (const key of Object.keys(obj)
      .filter((k) => obj[k] !== undefined)
      .sort()) {
      out.push(`${JSON.stringify(key)}:${canonicalizeJson(obj[key])}`);
    }
    return `{${out.join(",")}}`;
  }
  return JSON.stringify(value);
}

/** Re-export the receipt key lookup (for the /attestation command). */
export function receiptSigningKeys(keyset: WorkloadKeyset | undefined): KeysetKey[] {
  return Array.isArray(keyset?.receipt_signing_keys) ? keyset.receipt_signing_keys : [];
}

/** Generate a fresh nonce (32 random bytes, 64 lowercase hex — §3.2). */
export function newNonce(): string {
  return randomBytes(32).toString("hex");
}