/**
 * Level 2 report-binding checks (§10.1 checks 2–6), minus the hardware root of
 * trust. This verifies the *cryptographic binding* of the report — that its
 * `workload_id`, keyset digest, `report_data`, and endorsement are internally
 * consistent and endorsed by the identity key — for the nonce the client
 * supplied. It does NOT do §10.1 check 1 (the TEE quote verifies to the vendor
 * root) or the "hardware evidence binds `report_data`" half of check 4: parsing
 * and checking a TDX/SEV-SNP quote is verifier-profile territory and needs
 * primitives outside the Web Crypto API. Compose this with a quote verifier and
 * the custody/provenance/channel checks (§10.1 checks 1, 7–10) for full Level 2.
 */

import {
  computeWorkloadId,
  computeKeysetDigest,
  computeReportData,
  keysetEndorsementPayload,
} from './digest.js';
import { verifySignature, fromHex } from './crypto.js';
import type { AttestationReport, Check, ReportVerification } from './types.js';

/** Options for {@link verifyReportBinding}. */
export interface ReportBindingOptions {
  /**
   * Current time in Unix seconds for the freshness check (§10.1 check 6).
   * Defaults to the local clock. Pass an explicit value for deterministic tests.
   */
  now?: number;
  /**
   * Whether the profile trusts the platform's declared validity window
   * (`freshness.fetched_at`/`stale_after`, §5.1). Off by default — recency comes
   * from the nonce binding, and `fetched_at`/`stale_after` need a securely
   * synced TEE clock to mean anything.
   */
  trustPlatformClock?: boolean;
}

/**
 * Verify the report's cryptographic bindings for `nonce` (§10.1 checks 2–6).
 * `nonce` is the value the verifier supplied to `GET /v1/aci/attestation`, or
 * `null`/`undefined` when it requested no nonce (§4.4).
 *
 * Returns a per-check result and the identity recomputed from the report's
 * keyset; a failed check is `ok: false`, never thrown. Throws
 * {@link UnsupportedAlgorithmError} when the identity key algorithm is outside
 * Web Crypto scope (e.g. `ecdsa-secp256k1`).
 */
export async function verifyReportBinding(
  report: AttestationReport,
  nonce: string | null | undefined,
  options: ReportBindingOptions = {},
): Promise<ReportVerification> {
  const now = options.now ?? Math.floor(Date.now() / 1000);
  const checks: Check[] = [];

  const keyset = report.attestation.workload_keyset;
  const identityKey = keyset.workload_identity.public_key;

  // Check 2: workload_id == digest of the identity public key in the report's keyset.
  const workloadId = await computeWorkloadId(identityKey);
  pushEqual(checks, 'workload_id', report.workload_id, workloadId);

  // Check 3: workload_keyset_digest == digest of the report's keyset.
  const workloadKeysetDigest = await computeKeysetDigest(keyset);
  pushEqual(checks, 'workload_keyset_digest', report.workload_keyset_digest, workloadKeysetDigest);

  // Check 4 (binding half): report_data == the §4.4 statement digest for this nonce.
  // The hardware-evidence-binds-report_data half is out of scope (see file header).
  const expectedReportData = await computeReportData(workloadId, workloadKeysetDigest, nonce);
  pushEqual(checks, 'report_data', report.attestation.report_data, expectedReportData);

  // Check 5: keyset endorsement verifies under the identity key, algo matching.
  const endorsement = report.attestation.keyset_endorsement;
  if (endorsement.algo !== identityKey.algo) {
    checks.push({
      name: 'keyset_endorsement',
      ok: false,
      detail: `endorsement.algo "${endorsement.algo}" != identity key algo "${identityKey.algo}"`,
    });
  } else {
    const ok = await verifySignature(
      identityKey.algo,
      fromHex(identityKey.public_key),
      fromHex(endorsement.value),
      keysetEndorsementPayload(workloadKeysetDigest),
      'keyset endorsement (§4.3)',
    );
    checks.push({
      name: 'keyset_endorsement',
      ok,
      ...(ok ? {} : { detail: 'endorsement signature failed under identity key' }),
    });
  }

  // Check 6: freshness. Nonce binding is check 4; here bound the epoch and,
  // when trusted, the declared validity window.
  const notAfter = keyset.keyset_epoch.not_after;
  const epochOk = now < notAfter;
  checks.push({
    name: 'keyset_epoch.not_after',
    ok: epochOk,
    ...(epochOk ? {} : { detail: `now ${now} >= not_after ${notAfter}` }),
  });
  if (options.trustPlatformClock) {
    const freshness = report.attestation.freshness;
    const fetchedAt = freshness?.fetched_at;
    const staleAfter = freshness?.stale_after;
    const windowOk =
      typeof fetchedAt === 'number' &&
      typeof staleAfter === 'number' &&
      fetchedAt <= now &&
      now < staleAfter;
    checks.push({
      name: 'freshness_window',
      ok: windowOk,
      ...(windowOk ? {} : { detail: `now ${now} outside [${fetchedAt}, ${staleAfter})` }),
    });
  }

  return { ok: checks.every((c) => c.ok), checks, workloadId, workloadKeysetDigest };
}

function pushEqual(checks: Check[], name: string, actual: string, expected: string): void {
  const ok = actual === expected;
  checks.push({ name, ok, ...(ok ? {} : { detail: `report ${actual} != recomputed ${expected}` }) });
}
