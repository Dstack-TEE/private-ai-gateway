/**
 * Wire shapes for the ACI artifacts this verifier reads, plus the result types
 * it returns. These mirror spec/aci.md §3, §4, §7, §8; only the fields the
 * verifier touches are typed precisely, with an index signature left open so
 * extension fields (Appendix B) are visible to callers.
 */

/** A keyed public-key entry (§3.1) — receipt signing and E2EE keys. */
export interface KeysetKey {
  key_id: string;
  algo: string;
  public_key: string;
  [key: string]: unknown;
}

/** A TLS pin entry (§3.1): the certificate SPKI digest, optionally domain-scoped. */
export interface TlsKeyPin {
  spki_sha256: string;
  domain?: string;
  [key: string]: unknown;
}

/**
 * The workload keyset (§3.1) — the unit of workload identity. It travels as
 * `workload_keyset`; its digest is the SHA-256 of the JCS form of the
 * parsed object (Appendix A).
 */
export interface WorkloadKeyset {
  subject?: string | null;
  not_after: number;
  receipt_signing_keys: KeysetKey[];
  e2ee_public_keys: KeysetKey[];
  tls_public_keys?: TlsKeyPin[];
  [key: string]: unknown;
}

/** Source provenance (§4.1); each field is `null` when unknown. */
export interface SourceProvenance {
  repo_url?: string | null;
  repo_commit?: string | null;
  image_digest?: string | null;
  image_provenance?: unknown;
  [key: string]: unknown;
}

/** The `attestation` object of a report (§4.1). `evidence` is policy-defined (§4.2). */
export interface Attestation {
  tee_type: string;
  workload_keyset: unknown;
  report_data: string;
  source_provenance?: SourceProvenance | null;
  evidence?: unknown;
  [key: string]: unknown;
}

/** An attestation report (§4.1). */
export interface AttestationReport {
  api_version: string;
  workload_keyset_digest: string;
  attestation: Attestation;
  service_capabilities?: { supported_e2ee_versions?: string[]; [key: string]: unknown };
  [key: string]: unknown;
}

/**
 * The receipt document served by `GET /v1/aci/receipts/{id}` (§7.2): the
 * §7.3 payload members plus `key_id` and `signature`. The signature covers
 * JCS(document minus `signature`).
 */
export interface ReceiptEnvelope {
  key_id: string;
  signature: string;
  [key: string]: unknown;
}

/** A receipt event (§7.3): `type` plus type-specific fields; order is array order. */
export interface ReceiptEvent {
  type: string;
  body_hash?: string;
  [key: string]: unknown;
}

/** The receipt payload the envelope signs (§7.3). */
export interface ReceiptPayload {
  api_version: string;
  receipt_id: string;
  chat_id?: string | null;
  model?: string | null;
  workload_keyset_digest: string;
  endpoint: string;
  method: string;
  served_at: number;
  event_log: ReceiptEvent[];
  [key: string]: unknown;
}

/** A session evidence block (§8.2): a base64 data URI plus the digest of its decoded bytes. */
export interface SessionEvidence {
  digest: string;
  data?: string;
  [key: string]: unknown;
}

/**
 * An attested session record (§8.2). Its id is the SHA-256 of the JCS form
 * of the parsed document ({@link computeSessionId}).
 */
export interface SessionRecord {
  api_version: string;
  upstream_name: string;
  endpoint?: string | null;
  verifier_id: string;
  established_at: number;
  expires_at: number;
  identity?: unknown;
  channel_binding: unknown[];
  claims: unknown;
  evidence: SessionEvidence;
  [key: string]: unknown;
}

/** Outcome of one named verification check. */
export interface Check {
  /** Stable machine-readable id, e.g. `signature`, `report_data`. */
  name: string;
  ok: boolean;
  /** Human-readable detail, present when the check fails. */
  detail?: string;
}

/** Result of {@link verifyReceipt}: overall pass plus the individual §9.3 checks. */
export interface ReceiptVerification {
  ok: boolean;
  checks: Check[];
  /** The receipt document read as its §7.3 payload members. */
  payload?: ReceiptPayload;
}

/**
 * Result of {@link verifyReportBinding}: overall pass, the checks, and the
 * keyset established from the report — the digest is recomputed over the
 * served keyset object's JCS form (§3.1).
 */
export interface ReportVerification {
  ok: boolean;
  checks: Check[];
  workloadKeysetDigest?: string;
  keyset?: WorkloadKeyset;
}
