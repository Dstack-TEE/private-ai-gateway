// Receipt / attestation / session fetch, verification, and classification.
//
// Verification layers (mirrors gateway `examples/verify_aci_artifacts.rs`):
//   1. Attestation report binding (workload_id / keyset_digest / report_data /
//      keyset_endorsement / freshness) — recomputed from the keyset, not trusted
//      from the report's self-asserted fields.
//   2. Receipt signature — ed25519 (64-byte RFC 8032) over JCS(receipt with
//      signature.value omitted). Secp256k1 receipt signatures are no longer
//      supported (the gateway now signs with ed25519 only).
//   3. Workload match — receipt.workload_id / keyset_digest match the validated
//      attestation.
//   4. Body hashes — request.received.body_hash and response.returned.wire_hash
//      match the bytes the client sent/received (best-effort; response bytes
//      require streamSimple).
//   5. upstream.verified classification — verified/routed/unknown.

import { canonicalize, parseAciJson, type Json } from "./canonical.ts";
import { sha256Hex, verifyReceiptSignatureEd25519 } from "./crypto.ts";
import { verify as secpVerify } from "@noble/secp256k1";
import { sha256 as nobleSha256 } from "@noble/hashes/sha2.js";
import {
  DEFAULT_ATTESTATION_FETCH_TIMEOUT_MS,
  DEFAULT_RECEIPT_FETCH_TIMEOUT_MS,
  LOG_PREFIX,
  buildAttestationUrl,
  buildReceiptUrl,
  buildSessionUrl,
} from "./constants.ts";

// ----------------------------------------------------------------------------
// Wire types (subset; unknown fields preserved where needed)
// ----------------------------------------------------------------------------

export interface ReceiptEvent {
  type?: unknown;
  seq?: unknown;
  [field: string]: unknown;
}

export interface ReceiptSignature {
  algo?: unknown;
  key_id?: unknown;
  value?: unknown;
}

export interface Receipt {
  api_version?: unknown;
  receipt_id?: unknown;
  chat_id?: unknown;
  /** Top-level model id echoed by the gateway; part of the signed record (§8.5). */
  model?: unknown;
  workload_id?: unknown;
  workload_keyset_digest?: unknown;
  endpoint?: unknown;
  method?: unknown;
  served_at?: unknown;
  event_log?: unknown;
  signature?: unknown;
}

export interface KeyedPublicKey {
  key_id?: unknown;
  algo?: unknown;
  public_key?: unknown;
}

export interface WorkloadIdentity {
  public_key?: unknown;
  subject?: unknown;
}

export interface WorkloadKeyset {
  workload_identity?: unknown;
  keyset_epoch?: unknown;
  receipt_signing_keys?: unknown;
  e2ee_public_keys?: unknown;
  tls_public_keys?: unknown;
}

export interface AttestationReport {
  api_version?: unknown;
  workload_id?: unknown;
  workload_keyset_digest?: unknown;
  attestation?: {
    tee_type?: unknown;
    vendor?: unknown;
    workload_keyset?: WorkloadKeyset;
    report_data?: unknown;
    keyset_endorsement?: { algo?: unknown; value?: unknown };
    freshness?: { fetched_at?: unknown; stale_after?: unknown };
    source_provenance?: unknown;
    evidence?: unknown;
  };
  service_capabilities?: unknown;
}

export interface AttestedSession {
  session_id?: unknown;
  provider?: unknown;
  endpoint?: unknown;
  channel_binding?: unknown;
  claims?: unknown;
  established_at?: unknown;
  expires_at?: unknown;
}

// ----------------------------------------------------------------------------
// Canonical projections (field order MUST match gateway types.rs)
// ----------------------------------------------------------------------------

function publicKeyMaterialCanonical(pk: { algo?: unknown; public_key?: unknown }): Json {
  return { algo: pk.algo ?? null, public_key: pk.public_key ?? null };
}

function keyedPublicKeyCanonical(k: KeyedPublicKey): Json {
  return { key_id: k.key_id ?? null, algo: k.algo ?? null, public_key: k.public_key ?? null };
}

function workloadIdentityCanonical(identity: WorkloadIdentity): Json {
  const pk = (identity.public_key ?? {}) as { algo?: unknown; public_key?: unknown };
  return {
    public_key: publicKeyMaterialCanonical(pk),
    subject: identity.subject ?? null,
  };
}

function workloadKeysetCanonical(keyset: WorkloadKeyset): Json {
  const receiptKeys = Array.isArray(keyset.receipt_signing_keys)
    ? (keyset.receipt_signing_keys as KeyedPublicKey[]).map(keyedPublicKeyCanonical)
    : [];
  const e2eeKeys = Array.isArray(keyset.e2ee_public_keys)
    ? (keyset.e2ee_public_keys as KeyedPublicKey[]).map(keyedPublicKeyCanonical)
    : [];
  const tlsKeys = Array.isArray(keyset.tls_public_keys)
    ? (keyset.tls_public_keys as unknown[]).map((t) => {
        const tk = t as { spki_sha256?: unknown; domain?: unknown };
        const out: Record<string, unknown> = { spki_sha256: tk.spki_sha256 ?? null };
        if (tk.domain !== undefined) out.domain = tk.domain;
        return out;
      })
    : [];
  const epoch = (keyset.keyset_epoch ?? {}) as { version?: unknown; not_after?: unknown };
  return {
    workload_identity: workloadIdentityCanonical(
      (keyset.workload_identity ?? {}) as WorkloadIdentity,
    ),
    keyset_epoch: { version: epoch.version ?? null, not_after: epoch.not_after ?? null },
    receipt_signing_keys: receiptKeys,
    e2ee_public_keys: e2eeKeys,
    tls_public_keys: tlsKeys,
  };
}

function attestationStatementCanonical(
  workloadId: string,
  keysetDigest: string,
  nonce: string | null,
): Json {
  return {
    purpose: "aci.report_data.v1",
    workload_id: workloadId,
    workload_keyset_digest: keysetDigest,
    nonce,
  };
}

function keysetEndorsementPayloadCanonical(keysetDigest: string): Json {
  return { purpose: "aci.keyset.endorsement.v1", workload_keyset_digest: keysetDigest };
}

/**
 * Bytes the gateway signs (and a verifier must check) for a receipt: the JCS
 * canonicalization of the full wire record with only `signature.value`
 * removed (`algo`/`key_id` and any other signature fields stay), matching
 * ACI §8.5 and the reference verifier (`{ ...receipt, signature }` minus the
 * value). The record is spread as-is so unknown top-level fields (e.g.
 * `model`) and absent optional fields are signed byte-exactly as received.
 */
export function canonicalBytesForSigning(receipt: Receipt): Uint8Array {
  const record = receipt as unknown as Record<string, unknown>;
  const signature = (receipt.signature ?? {}) as Record<string, unknown>;
  const { value: _omitted, ...signatureWithoutValue } = signature;
  return canonicalize({ ...record, signature: signatureWithoutValue });
}

// ----------------------------------------------------------------------------
// Identity / report binding (gateway identity.rs + verifier/report.rs)
// ----------------------------------------------------------------------------

function asString(value: unknown): string | undefined {
  return typeof value === "string" ? value : undefined;
}

function asBigInt(value: unknown): bigint | undefined {
  if (typeof value === "bigint") return value;
  if (typeof value === "number" && Number.isInteger(value)) return BigInt(value);
  return undefined;
}

function hexToBytesLocal(value: string): Uint8Array {
  const stripped = value.startsWith("0x") ? value.slice(2) : value;
  if (stripped.length % 2 !== 0) throw new Error(`invalid hex length: ${stripped.length}`);
  const out = new Uint8Array(stripped.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(stripped.substr(i * 2, 2), 16);
  }
  return out;
}

/** `sha256:hex(sha256(JCS(value)))`. */
function jcsSha256Hex(value: Json): string {
  return sha256Hex(canonicalize(value));
}

/** Recompute workload_id from the keyset's identity public key. */
export function computeWorkloadId(keyset: WorkloadKeyset): string {
  const identity = (keyset.workload_identity ?? {}) as WorkloadIdentity;
  const pk = (identity.public_key ?? {}) as { algo?: unknown; public_key?: unknown };
  return jcsSha256Hex(publicKeyMaterialCanonical(pk));
}

/** Recompute workload_keyset_digest from the full keyset. */
export function computeWorkloadKeysetDigest(keyset: WorkloadKeyset): string {
  return jcsSha256Hex(workloadKeysetCanonical(keyset));
}

/**
 * Find the attested SPKI SHA-256 for a host from a validated attestation's
 * `workload_keyset.tls_public_keys`. Used for TLS SPKI pinning: a host whose
 * SPKI is attested here is served by a key cryptographically bound to the
 * attested workload (§ attestation keyset). Returns undefined when the report
 * does not list the host.
 */
export function attestedSpkiSha256ForHost(
  report: AttestationReport,
  host: string,
): string | undefined {
  const keys = report.attestation?.workload_keyset?.tls_public_keys;
  if (!Array.isArray(keys)) return undefined;
  const normalized = host.toLowerCase();
  for (const entry of keys as Array<{ domain?: unknown; spki_sha256?: unknown }>) {
    if (typeof entry?.domain === "string" && entry.domain.toLowerCase() === normalized) {
      if (typeof entry.spki_sha256 === "string" && entry.spki_sha256.length > 0) {
        return entry.spki_sha256;
      }
    }
  }
  return undefined;
}

export interface ReportBindingResult {
  workloadId: string;
  workloadKeysetDigest: string;
  reportData: Uint8Array;
}

export class ReportBindingError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ReportBindingError";
  }
}

/**
 * Validate ACI attestation report binding: recompute workload_id and
 * keyset_digest from the published keyset, verify the report_data binds the
 * nonce and keyset, verify the keyset_endorsement under the identity key, and
 * check freshness. Does NOT verify the vendor TEE quote (that needs Intel DCAP
 * collateral; documented as out of scope for this client).
 */
export function validateAciReportBinding(
  report: AttestationReport,
  nonce: string | null,
  nowSecs: number,
): ReportBindingResult {
  if (asString(report.api_version) !== "aci/1") {
    throw new ReportBindingError(`unsupported ACI api_version: ${String(report.api_version)}`);
  }
  const envelope = report.attestation;
  if (!envelope) throw new ReportBindingError("attestation envelope missing");
  const keyset = (envelope.workload_keyset ?? {}) as WorkloadKeyset;
  if (!keyset.workload_identity) throw new ReportBindingError("workload_keyset missing identity");

  const computedWorkloadId = computeWorkloadId(keyset);
  if (computedWorkloadId !== asString(report.workload_id)) {
    throw new ReportBindingError("workload_id mismatch");
  }
  const computedKeysetDigest = computeWorkloadKeysetDigest(keyset);
  const reportedKeysetDigest = asString(report.workload_keyset_digest);
  if (computedKeysetDigest !== reportedKeysetDigest) {
    throw new ReportBindingError(
      `workload_keyset_digest mismatch (computed=${computedKeysetDigest}, reported=${reportedKeysetDigest ?? "undefined"})`,
    );
  }

  const statement = attestationStatementCanonical(computedWorkloadId, computedKeysetDigest, nonce);
  const expectedReportData = new Uint8Array(
    hexToBytesLocal(jcsSha256Hex(statement).slice("sha256:".length)),
  );
  const reportedHex = asString(envelope.report_data);
  if (!reportedHex) throw new ReportBindingError("report_data missing");
  const reported = hexToBytesLocal(reportedHex);
  if (reported.length !== 32 || !timingSafeEqual(reported, expectedReportData)) {
    throw new ReportBindingError("report_data mismatch");
  }

  const endorsement = envelope.keyset_endorsement ?? {};
  const identityPk = ((keyset.workload_identity as WorkloadIdentity).public_key ?? {}) as {
    algo?: unknown;
    public_key?: unknown;
  };
  if (asString(endorsement.algo) !== asString(identityPk.algo)) {
    throw new ReportBindingError("keyset_endorsement algo mismatch");
  }
  const endorsementPayload = canonicalize(
    keysetEndorsementPayloadCanonical(computedKeysetDigest),
  );
  // keyset_endorsement: ecdsa-secp256k1 64-byte r||s over sha256(payload), or
  // ed25519 64-byte over payload. We only verify secp256k1 (the documented
  // gateway algo); ed25519 (identity algos) would need a separate path.
  if (asString(identityPk.algo) === "ecdsa-secp256k1") {
    if (!verifyKeysetEndorsementSecp256k1(identityPk, endorsementPayload, endorsement)) {
      throw new ReportBindingError("keyset_endorsement signature verification failed");
    }
  }

  const freshness = envelope.freshness ?? {};
  const fetchedAt = asBigInt(freshness.fetched_at);
  const staleAfter = asBigInt(freshness.stale_after);
  const now = BigInt(nowSecs);
  if (
    fetchedAt === undefined ||
    staleAfter === undefined ||
    now < fetchedAt ||
    now >= staleAfter
  ) {
    throw new ReportBindingError("attestation report is not fresh at verifier time");
  }

  return {
    workloadId: computedWorkloadId,
    workloadKeysetDigest: computedKeysetDigest,
    reportData: expectedReportData,
  };
}

function timingSafeEqual(a: Uint8Array, b: Uint8Array): boolean {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
}

// keyset_endorsement uses 64-byte r||s (non-recoverable) over sha256(payload).
// noble's verify() handles this.

function verifyKeysetEndorsementSecp256k1(
  identityPk: { public_key?: unknown },
  payload: Uint8Array,
  endorsement: { value?: unknown },
): boolean {
  const pubHex = asString(identityPk.public_key);
  const sigHex = asString(endorsement.value);
  if (!pubHex || !sigHex) return false;
  try {
    const pubBytes = hexToBytesLocal(pubHex);
    const sig = hexToBytesLocal(sigHex);
    if (sig.length !== 64) return false;
    // Gateway signs sha256(payload) using a plain 64-byte r||s signature.
    const prehash = nobleSha256(payload);
    return secpVerify(sig, prehash, pubBytes, { prehash: false } as never);
  } catch {
    return false;
  }
}

// ----------------------------------------------------------------------------
// Receipt classification + full verification
// ----------------------------------------------------------------------------

export type ReceiptStatus = "verified" | "routed" | "unknown";

export interface ReceiptClassification {
  status: ReceiptStatus;
  provider?: string;
  modelId?: string;
  sessionId?: string;
  required?: boolean;
  workloadId?: string;
  keysetDigest?: string;
  /** Workload match against a validated attestation (set by verifyReceipt). */
  workloadMatched?: boolean;
  /** Receipt signature verification result (set by verifyReceipt). */
  signatureValid?: boolean;
  /** Request body hash match (set by verifyReceipt when requestBody provided). */
  requestHashValid?: boolean;
  /** Response wire hash match (set by verifyReceipt when responseBytes provided). */
  responseHashValid?: boolean;
}

function findUpstreamVerified(receipt: Receipt): Record<string, unknown> | undefined {
  const log = receipt.event_log;
  if (!Array.isArray(log)) return undefined;
  for (const e of log as ReceiptEvent[]) {
    if (e && asString(e.type) === "upstream.verified") return e as Record<string, unknown>;
  }
  return undefined;
}

export function classifyReceipt(receipt: Receipt): ReceiptClassification {
  const base: ReceiptClassification = {
    status: "unknown",
    workloadId: asString(receipt.workload_id),
    keysetDigest: asString(receipt.workload_keyset_digest),
  };
  const event = findUpstreamVerified(receipt);
  if (!event) return base;

  const result = asString(event.result);
  const required = event.required === true;
  const provider = asString(event.provider);
  const modelId = asString(event.model_id);
  const sessionId = asString(event.session_id);

  if (result === "verified" && required) {
    return { ...base, status: "verified", provider, modelId, sessionId, required };
  }
  if (result === "failed" && !required) {
    return { ...base, status: "routed", provider, modelId, sessionId, required };
  }
  return { ...base, status: "unknown", provider, modelId, sessionId, required };
}

function findEvent(receipt: Receipt, type: string): Record<string, unknown> | undefined {
  const log = receipt.event_log;
  if (!Array.isArray(log)) return undefined;
  for (const e of log as ReceiptEvent[]) {
    if (e && asString(e.type) === type) return e as Record<string, unknown>;
  }
  return undefined;
}

/**
 * Full receipt verification against a validated attestation report.
 * Returns a classification with all check fields populated.
 */
export function verifyReceipt(
  receipt: Receipt,
  binding: ReportBindingResult,
  attestation: AttestationReport,
  options: { requestBody?: Uint8Array; responseBytes?: Uint8Array } = {},
): ReceiptClassification {
  const classification = classifyReceipt(receipt);

  // Workload match.
  classification.workloadMatched =
    classification.workloadId === binding.workloadId &&
    classification.keysetDigest === binding.workloadKeysetDigest;

  // Signature verification.
  const sig = (receipt.signature ?? {}) as ReceiptSignature;
  const sigValue = asString(sig.value);
  const sigKeyId = asString(sig.key_id);
  const keyset = (attestation.attestation?.workload_keyset ?? {}) as WorkloadKeyset;
  const receiptKeys = Array.isArray(keyset.receipt_signing_keys)
    ? (keyset.receipt_signing_keys as KeyedPublicKey[])
    : [];
  const receiptKey = receiptKeys.find((k) => asString(k.key_id) === sigKeyId);
  if (receiptKey && sigValue) {
    const algo = asString(receiptKey.algo);
    const pub = asString(receiptKey.public_key) ?? "";
    try {
      const canonical = canonicalBytesForSigning(receipt);
      const signature = hexToBytesLocal(sigValue);
      if (algo === "ed25519") {
        classification.signatureValid = verifyReceiptSignatureEd25519(pub, canonical, signature);
      } else {
        // Only ed25519 receipt signatures are supported (ACI §8.5, RECOMMENDED).
        classification.signatureValid = false;
      }
    } catch {
      classification.signatureValid = false;
    }
  } else {
    classification.signatureValid = false;
  }

  // Request body hash.
  if (options.requestBody) {
    const expected = sha256Hex(options.requestBody);
    const event = findEvent(receipt, "request.received");
    classification.requestHashValid = asString(event?.body_hash) === expected;
  }

  // Response wire hash.
  if (options.responseBytes) {
    const expected = sha256Hex(options.responseBytes);
    const event = findEvent(receipt, "response.returned");
    const cleartext = asString(event?.cleartext_hash);
    const wire = asString(event?.wire_hash);
    classification.responseHashValid = cleartext === expected || wire === expected;
  }

  return classification;
}

/** Overall verified = workload matched + signature valid + (hashes if provided). */
export function isFullyVerified(classification: ReceiptClassification): boolean {
  if (!classification.workloadMatched) return false;
  if (classification.signatureValid === false) return false;
  if (classification.requestHashValid === false) return false;
  if (classification.responseHashValid === false) return false;
  return true;
}

// ----------------------------------------------------------------------------
// Network fetches
// ----------------------------------------------------------------------------

export function attestationStaleAfter(report: AttestationReport): bigint | undefined {
  const stale = report.attestation?.freshness?.stale_after;
  return typeof stale === "bigint" ? stale : undefined;
}

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
      return parseAciJson(text);
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
): Promise<Receipt | null> {
  const json = await fetchJson(
    buildReceiptUrl(receiptId, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_RECEIPT_FETCH_TIMEOUT_MS,
    `receipt ${receiptId}`,
  );
  return json as Receipt | null;
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
): Promise<AttestedSession | null> {
  const json = await fetchJson(
    buildSessionUrl(sessionId, options.baseUrl),
    apiKey,
    options.timeoutMs ?? DEFAULT_RECEIPT_FETCH_TIMEOUT_MS,
    `session ${sessionId}`,
  );
  return json as AttestedSession | null;
}
