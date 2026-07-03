import { test } from 'node:test';
import assert from 'node:assert/strict';
import { verifyReportBinding, UnsupportedAlgorithmError, type AttestationReport } from '../src/index.js';
import * as fx from './fixtures.js';

/** A report assembled from the §1–§4 vectors: identity, keyset, report_data for `test-nonce`, endorsement. */
function makeReport(): AttestationReport {
  return {
    api_version: 'aci/1',
    workload_id: fx.WORKLOAD_ID,
    workload_keyset_digest: fx.KEYSET_DIGEST,
    attestation: {
      workload_keyset: structuredClone(fx.KEYSET),
      report_data: fx.REPORT_DATA_WITH_NONCE,
      keyset_endorsement: { algo: 'ed25519', value: fx.ENDORSEMENT_SIGNATURE },
      freshness: { fetched_at: 1750000000, stale_after: 1750003600 },
    },
  };
}

const NOW = 1750001000; // inside the freshness window, before not_after

test('§10.1 checks 2–6: a well-formed report binding passes', async () => {
  const result = await verifyReportBinding(makeReport(), fx.ATTESTATION_NONCE, { now: NOW });
  assert.equal(result.ok, true, JSON.stringify(result.checks));
  assert.equal(result.workloadId, fx.WORKLOAD_ID);
  assert.equal(result.workloadKeysetDigest, fx.KEYSET_DIGEST);
});

test('report_data check fails for a different nonce (freshness, check 4/6)', async () => {
  const result = await verifyReportBinding(makeReport(), 'other-nonce', { now: NOW });
  assert.equal(result.ok, false);
  assert.equal(result.checks.find((c) => c.name === 'report_data')?.ok, false);
});

test('report_data check accepts the omitted-nonce report', async () => {
  const report = makeReport();
  report.attestation.report_data = fx.REPORT_DATA_NONCE_OMITTED;
  const result = await verifyReportBinding(report, undefined, { now: NOW });
  assert.equal(result.checks.find((c) => c.name === 'report_data')?.ok, true);
});

test('an expired keyset epoch fails check 6', async () => {
  const result = await verifyReportBinding(makeReport(), fx.ATTESTATION_NONCE, { now: 1800000001 });
  assert.equal(result.ok, false);
  assert.equal(result.checks.find((c) => c.name === 'keyset_epoch.not_after')?.ok, false);
});

test('trustPlatformClock enforces the declared validity window', async () => {
  const ok = await verifyReportBinding(makeReport(), fx.ATTESTATION_NONCE, {
    now: NOW,
    trustPlatformClock: true,
  });
  assert.equal(ok.checks.find((c) => c.name === 'freshness_window')?.ok, true);

  const stale = await verifyReportBinding(makeReport(), fx.ATTESTATION_NONCE, {
    now: 1750009999, // past stale_after
    trustPlatformClock: true,
  });
  assert.equal(stale.checks.find((c) => c.name === 'freshness_window')?.ok, false);
});

test('a tampered endorsement fails check 5', async () => {
  const report = makeReport();
  report.attestation.keyset_endorsement.value = report.attestation.keyset_endorsement.value.replace(
    /^../,
    '00',
  );
  const result = await verifyReportBinding(report, fx.ATTESTATION_NONCE, { now: NOW });
  assert.equal(result.ok, false);
  assert.equal(result.checks.find((c) => c.name === 'keyset_endorsement')?.ok, false);
});

test('endorsement.algo must match the identity key algo', async () => {
  const report = makeReport();
  report.attestation.keyset_endorsement.algo = 'ecdsa-secp256k1';
  const result = await verifyReportBinding(report, fx.ATTESTATION_NONCE, { now: NOW });
  assert.equal(result.checks.find((c) => c.name === 'keyset_endorsement')?.ok, false);
});

test('a secp256k1 identity key is out of scope: throws', async () => {
  const report = makeReport();
  report.attestation.workload_keyset.workload_identity.public_key.algo = 'ecdsa-secp256k1';
  report.attestation.keyset_endorsement.algo = 'ecdsa-secp256k1';
  // workload_id/digest recomputation still uses whatever algo string is present;
  // the endorsement verify is where the unsupported algorithm surfaces.
  await assert.rejects(
    () => verifyReportBinding(report, fx.ATTESTATION_NONCE, { now: NOW }),
    UnsupportedAlgorithmError,
  );
});
