/**
 * Wire shapes for the ACI artifacts this verifier reads, plus the result types
 * it returns. These mirror spec/aci.md §4, §5, §8, §9; only the fields the
 * verifier touches are typed precisely, with an index signature left open so
 * unknown extension fields (§3.2) survive canonicalization untouched.
 */

import type { JcsValue } from './jcs.js';

/** A public key object: `{ algo, public_key }` (§4.1). */
export interface PublicKey {
  algo: string;
  public_key: string;
  [key: string]: JcsValue | undefined;
}

/** The workload identity (§4.2): the identity public key plus an optional subject. */
export interface WorkloadIdentity {
  public_key: PublicKey;
  subject?: string | null;
  [key: string]: JcsValue | undefined;
}

/** A receipt signing key entry (§4.2). */
export interface ReceiptSigningKey {
  key_id: string;
  algo: string;
  public_key: string;
  [key: string]: JcsValue | undefined;
}

/** The workload keyset (§4.2). Digested with JCS to yield `workload_keyset_digest`. */
export interface WorkloadKeyset {
  workload_identity: WorkloadIdentity;
  keyset_epoch: { version: number; not_after: number; [key: string]: JcsValue | undefined };
  receipt_signing_keys: ReceiptSigningKey[];
  e2ee_public_keys?: JcsValue[];
  tls_public_keys?: JcsValue[];
  [key: string]: JcsValue | undefined;
}

/** A receipt signature block (§8.2). `value` is dropped for canonical signing bytes (§8.5). */
export interface ReceiptSignature {
  algo: string;
  key_id: string;
  value: string;
  [key: string]: JcsValue | undefined;
}

/** A single receipt event (§8.3). Only `seq`/`type` are fixed; other fields are type-specific. */
export interface ReceiptEvent {
  seq: number;
  type: string;
  [key: string]: JcsValue | undefined;
}

/** An inference receipt (§8.2). */
export interface Receipt {
  api_version: string;
  receipt_id: string;
  workload_id: string;
  workload_keyset_digest: string;
  event_log: ReceiptEvent[];
  signature: ReceiptSignature;
  [key: string]: JcsValue | undefined;
}

/** The keyset endorsement / revocation signature block (§4.3, §5.1). */
export interface Endorsement {
  algo: string;
  value: string;
  [key: string]: JcsValue | undefined;
}

/** The `attestation` object of a report (§5.1); only the fields Level 1 reads are typed. */
export interface Attestation {
  workload_keyset: WorkloadKeyset;
  report_data: string;
  keyset_endorsement: Endorsement;
  freshness?: { fetched_at?: number; stale_after?: number; [key: string]: JcsValue | undefined };
  [key: string]: JcsValue | undefined;
}

/** An attestation report (§5.1). */
export interface AttestationReport {
  api_version: string;
  workload_id: string;
  workload_keyset_digest: string;
  attestation: Attestation;
  [key: string]: JcsValue | undefined;
}

/** A verifier-provided evidence block on a session record (§9.2). */
export interface SessionEvidence {
  digest?: string | null;
  data?: string;
  [key: string]: JcsValue | undefined;
}

/**
 * An attested session record (§9.2). The `session_id` is recomputed from the
 * named fields; absent optional fields (`endpoint`, `identity`, `evidence.digest`)
 * are restored as JSON `null` in the content-addressing material.
 */
export interface SessionRecord {
  upstream_name: string;
  endpoint?: string | null;
  verifier_id: string;
  identity?: JcsValue;
  channel_binding: JcsValue[];
  claims: JcsValue;
  evidence?: SessionEvidence | null;
  [key: string]: JcsValue | undefined;
}

/** Outcome of one named verification check. */
export interface Check {
  /** Stable machine-readable id, e.g. `signature`, `workload_id`. */
  name: string;
  ok: boolean;
  /** Human-readable detail, present when the check fails. */
  detail?: string;
}

/** Result of {@link verifyReceipt}: overall pass plus the individual §10.2 checks. */
export interface ReceiptVerification {
  ok: boolean;
  checks: Check[];
}

/** Result of {@link verifyReportBinding}: overall pass, the §10.1 checks, and the derived identity. */
export interface ReportVerification {
  ok: boolean;
  checks: Check[];
  /** `workload_id` recomputed from the report's keyset (§4.1). */
  workloadId: string;
  /** `workload_keyset_digest` recomputed from the report's keyset (§4.2). */
  workloadKeysetDigest: string;
}
