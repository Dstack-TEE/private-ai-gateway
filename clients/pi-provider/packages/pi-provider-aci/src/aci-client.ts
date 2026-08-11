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
// The report binding (verifyReportBinding) recomputes the keyset digest,
// checks report_data against our nonce, and checks not_after — so the SPKI we
// pin is established from a report that actually passed, not taken on trust.

import { randomBytes } from "node:crypto";

import {
  type AttestationReport,
  type TlsKeyPin,
  type WorkloadKeyset,
  AciError,
  verifyReportBinding,
} from "@phala/aci-verifier";

import {
  DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
  LOG_PREFIX,
  buildAttestationUrl,
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
// Binding (verifier-ts §9.1 checks 2–3) — the prevention foundation
// ----------------------------------------------------------------------------

/** Validate an attestation report against the nonce we sent. Returns null when
 *  the binding fails or the report is malformed — never throws for a failed
 *  check (only for a bad caller nonce, which we always generate). */
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