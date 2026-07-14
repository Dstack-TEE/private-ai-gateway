/**
 * Wire shapes for the ACI artifacts this verifier reads, plus the result types
 * it returns. These mirror spec/aci.md §4, §5, §8, §9; only the fields the
 * verifier touches are typed precisely, with an index signature left open so
 * extension fields (§3.1) are visible to callers.
 */

/** A keyed public-key entry (§4.1) — receipt signing and E2EE keys. */
export interface KeysetKey {
  key_id: string;
  algo: string;
  public_key: string;
  [key: string]: unknown;
}

/** A TLS pin entry (§4.1): the certificate SPKI digest, optionally domain-scoped. */
export interface TlsKeyPin {
  spki_sha256: string;
  domain?: string;
  [key: string]: unknown;
}

/**
 * The workload keyset (§4.1) — the unit of workload identity. It travels as
 * `workload_keyset_b64`; its digest is over those exact decoded bytes, never a
 * re-serialization of this parsed form.
 */
export interface WorkloadKeyset {
  subject?: string | null;
  not_after: number;
  receipt_signing_keys: KeysetKey[];
  e2ee_public_keys: KeysetKey[];
  tls_public_keys?: TlsKeyPin[];
  [key: string]: unknown;
}

/** Source provenance (§5.1); each field is `null` when unknown. */
export interface SourceProvenance {
  repo_url?: string | null;
  repo_commit?: string | null;
  image_digest?: string | null;
  image_provenance?: unknown;
  [key: string]: unknown;
}

/** The `attestation` object of a report (§5.1). `evidence` is profile-defined (§5.2). */
export interface Attestation {
  tee_type: string;
  workload_keyset_b64: string;
  report_data: string;
  source_provenance?: SourceProvenance | null;
  evidence?: unknown;
  [key: string]: unknown;
}

/** An attestation report (§5.1). */
export interface AttestationReport {
  api_version: string;
  workload_keyset_digest: string;
  attestation: Attestation;
  service_capabilities?: { supported_e2ee_versions?: string[]; [key: string]: unknown };
  [key: string]: unknown;
}

/** The signed-bytes envelope served by `GET /v1/aci/receipts/{id}` (§8.2). */
export interface ReceiptEnvelope {
  payload_b64: string;
  key_id: string;
  algo: string;
  signature: string;
  [key: string]: unknown;
}

/** A receipt event (§8.3): `type` plus type-specific fields; order is array order. */
export interface ReceiptEvent {
  type: string;
  body_hash?: string;
  [key: string]: unknown;
}

/** The receipt payload the envelope signs (§8.3). */
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

/** A session evidence block (§9.2): a base64 data URI plus the digest of its decoded bytes. */
export interface SessionEvidence {
  digest: string;
  data?: string;
  [key: string]: unknown;
}

/**
 * An attested session record (§9.2). Its id is the SHA-256 of the exact served
 * document bytes ({@link computeSessionId}); this parsed form is for reading
 * the verification material, never for recomputing the id.
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

/** Result of {@link verifyReceipt}: overall pass plus the individual §10.2 checks. */
export interface ReceiptVerification {
  ok: boolean;
  checks: Check[];
  /** The parsed payload, present when `payload_b64` decoded to valid JSON. */
  payload?: ReceiptPayload;
}

/**
 * Result of {@link verifyReportBinding}: overall pass, the checks, and the
 * keyset established from the report — the digest is recomputed from the
 * decoded `workload_keyset_b64` bytes (§4.1), which are also returned.
 */
export interface ReportVerification {
  ok: boolean;
  checks: Check[];
  workloadKeysetDigest?: string;
  keysetBytes?: Uint8Array;
  keyset?: WorkloadKeyset;
}
