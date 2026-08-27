import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  verifyReportBinding,
  verifyComposeMeasurement,
  computeKeysetDigest,
  computeReportData,
  toBase64,
  type AttestationReport,
  type Check,
} from '../src/index.js';
import * as fx from './fixtures.js';

function check(checks: Check[], name: string): Check {
  const found = checks.find((c) => c.name === name);
  assert.ok(found, `missing check "${name}"`);
  return found;
}

test('§9.1 checks 2–3: a well-formed report passes and establishes the keyset', async () => {
  const result = await verifyReportBinding(fx.REPORT, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, true, JSON.stringify(result.checks));
  assert.deepEqual(
    result.checks.map((c) => c.name),
    ['api_version', 'workload_keyset_digest', 'report_data', 'not_after'],
  );
  assert.equal(result.workloadKeysetDigest, fx.KEYSET_DIGEST);
  assert.equal(result.keyset?.subject, 'dstack-app://example-app');
});

test('report_data fails for a different nonce — a stale quote cannot bind our challenge', async () => {
  const result = await verifyReportBinding(fx.REPORT, '11'.repeat(32), { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'report_data').ok, false);
});

test('an omitted-nonce report verifies with nonce null/undefined', async () => {
  const report = fx.makeReport(await computeReportData(fx.KEYSET_DIGEST, null));
  assert.equal((await verifyReportBinding(report, null, { now: fx.NOW })).ok, true);
  assert.equal((await verifyReportBinding(report, undefined, { now: fx.NOW })).ok, true);
});

test('a tampered keyset object fails both the digest and the statement recomputation', async () => {
  const report = structuredClone(fx.REPORT) as AttestationReport;
  (report.attestation.workload_keyset as Record<string, unknown>).not_after = 1_900_000_000;
  const result = await verifyReportBinding(report, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'workload_keyset_digest').ok, false);
  assert.equal(check(result.checks, 'report_data').ok, false);
});

test('the recomputed digest is authoritative: a tampered restated copy cannot move report_data', async () => {
  const report = structuredClone(fx.REPORT) as AttestationReport;
  report.workload_keyset_digest = 'sha256:' + '00'.repeat(32);
  const result = await verifyReportBinding(report, fx.NONCE, { now: fx.NOW });
  assert.equal(check(result.checks, 'workload_keyset_digest').ok, false);
  assert.equal(check(result.checks, 'report_data').ok, true);
});

test('an expired keyset fails check 3', async () => {
  const result = await verifyReportBinding(fx.REPORT, fx.NONCE, { now: fx.NOT_AFTER });
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'not_after').ok, false);
});

test('artifacts with another api_version are rejected (Appendix B)', async () => {
  const report = structuredClone(fx.REPORT) as AttestationReport;
  report.api_version = 'aci/2';
  const result = await verifyReportBinding(report, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'api_version').ok, false);
});

test('a non-object workload_keyset fails everything and establishes nothing', async () => {
  const report = structuredClone(fx.REPORT) as AttestationReport;
  (report.attestation as { workload_keyset: unknown }).workload_keyset = 'not an object';
  const result = await verifyReportBinding(report, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(result.keyset, undefined);
  assert.equal(result.workloadKeysetDigest, undefined);
  for (const name of ['workload_keyset_digest', 'report_data', 'not_after']) {
    assert.equal(check(result.checks, name).ok, false, name);
  }
});

test('§9.1 check 4: app_compose measured into the quote RTMR3 passes; a tampered compose fails', async () => {
  const { report, composeHash } = await fx.makeMeasuredComposeReport();

  const verified = await verifyComposeMeasurement(report);
  assert.equal(verified.ok, true);
  assert.equal(verified.composeHash, composeHash);

  // Tamper the running compose: sha256(app_compose) no longer matches, but the
  // RTMR3 replay (over the untouched event digests) still does.
  report.attestation.evidence = { ...(report.attestation.evidence as object), app_compose: 'tampered' };
  const bad = await verifyComposeMeasurement(report);
  assert.equal(bad.ok, false);
  assert.equal(check(bad.checks, 'compose_hash').ok, false);
  assert.equal(check(bad.checks, 'rtmr3').ok, true);
});

test('a keyset object without not_after still binds, but expiry fails closed', async () => {
  const keyset: Record<string, unknown> = { ...(fx.KEYSET as unknown as Record<string, unknown>) };
  delete keyset.not_after;
  const digest = await computeKeysetDigest(keyset);
  const report = structuredClone(fx.REPORT) as AttestationReport;
  report.workload_keyset_digest = digest;
  (report.attestation as { workload_keyset: unknown }).workload_keyset = keyset;
  report.attestation.report_data = await computeReportData(digest, fx.NONCE);
  const result = await verifyReportBinding(report, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'workload_keyset_digest').ok, true);
  assert.equal(check(result.checks, 'report_data').ok, true);
  assert.equal(check(result.checks, 'not_after').ok, false);
});
