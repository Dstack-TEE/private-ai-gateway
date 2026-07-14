import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  verifyReportBinding,
  verifyComposeMeasurement,
  computeKeysetDigest,
  computeReportData,
  sha256Hex,
  fromHex,
  toHex,
  toBase64,
  type AttestationReport,
  type Check,
} from '../src/index.js';
import { sha384 } from '../src/crypto.js';
import * as fx from './fixtures.js';

const enc = new TextEncoder();

function check(checks: Check[], name: string): Check {
  const found = checks.find((c) => c.name === name);
  assert.ok(found, `missing check "${name}"`);
  return found;
}

test('§10.1 checks 2–3: a well-formed report passes and establishes the keyset', async () => {
  const result = await verifyReportBinding(fx.REPORT, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, true, JSON.stringify(result.checks));
  assert.deepEqual(
    result.checks.map((c) => c.name),
    ['api_version', 'workload_keyset_digest', 'report_data', 'not_after'],
  );
  assert.equal(result.workloadKeysetDigest, fx.KEYSET_DIGEST);
  assert.deepEqual(result.keysetBytes, fx.KEYSET_BYTES);
  assert.equal(result.keyset?.subject, 'dstack-app://example-app');
});

test('report_data fails for a different nonce — a stale quote cannot bind our challenge', async () => {
  const result = await verifyReportBinding(fx.REPORT, 'other-nonce', { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'report_data').ok, false);
});

test('an omitted-nonce report verifies with nonce null/undefined', async () => {
  const report = fx.makeReport(await computeReportData(fx.KEYSET_DIGEST, null));
  assert.equal((await verifyReportBinding(report, null, { now: fx.NOW })).ok, true);
  assert.equal((await verifyReportBinding(report, undefined, { now: fx.NOW })).ok, true);
});

test('tampered keyset bytes fail both the digest and the statement recomputation', async () => {
  const report = structuredClone(fx.REPORT) as AttestationReport;
  report.attestation.workload_keyset_b64 = toBase64(
    enc.encode(new TextDecoder().decode(fx.KEYSET_BYTES) + ' '),
  );
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

test('artifacts with another api_version are rejected (Appendix A)', async () => {
  const report = structuredClone(fx.REPORT) as AttestationReport;
  report.api_version = 'aci/2';
  const result = await verifyReportBinding(report, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'api_version').ok, false);
});

test('undecodable workload_keyset_b64 fails everything and establishes nothing', async () => {
  const report = structuredClone(fx.REPORT) as AttestationReport;
  report.attestation.workload_keyset_b64 = '!not base64!';
  const result = await verifyReportBinding(report, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(result.keyset, undefined);
  assert.equal(result.workloadKeysetDigest, undefined);
  for (const name of ['workload_keyset_digest', 'report_data', 'not_after']) {
    assert.equal(check(result.checks, name).ok, false, name);
  }
});

test('§10.1 check 4: app_compose measured into the quote RTMR3 passes; a tampered compose fails', async () => {
  const appCompose = 'services:\n  gateway:\n    image: demo\n';
  const composeHash = await sha256Hex(enc.encode(appCompose));
  const digests = ['11'.repeat(48), '22'.repeat(48)];
  // Replay the imr==3 digests to RTMR3, then plant it at the v4 TDX offset so
  // the event log verifies against the (unauthenticated) quote body.
  let mr: Uint8Array = new Uint8Array(48);
  for (const d of digests) {
    const buf = new Uint8Array(mr.length + 48);
    buf.set(mr);
    buf.set(fromHex(d), mr.length);
    mr = await sha384(buf);
  }
  const quote = new Uint8Array(568);
  quote.set(mr, 520);
  const events = [
    { imr: 3, digest: digests[0], event: 'compose-hash', event_payload: composeHash },
    { imr: 3, digest: digests[1], event: 'system-ready', event_payload: '' },
  ];
  const report = {
    api_version: 'aci/1',
    workload_keyset_digest: 'sha256:' + '00'.repeat(32),
    attestation: {
      tee_type: 'tdx',
      workload_keyset_b64: '',
      report_data: '',
      evidence: { event_log: JSON.stringify(events), app_compose: appCompose, quote: toHex(quote) },
    },
  } as AttestationReport;

  assert.equal((await verifyComposeMeasurement(report)).ok, true);

  // Tamper the running compose: sha256(app_compose) no longer matches, but the
  // RTMR3 replay (over the untouched event digests) still does.
  report.attestation.evidence = { ...(report.attestation.evidence as object), app_compose: 'tampered' };
  const bad = await verifyComposeMeasurement(report);
  assert.equal(bad.ok, false);
  assert.equal(check(bad.checks, 'compose_hash').ok, false);
  assert.equal(check(bad.checks, 'rtmr3').ok, true);
});

test('the bytes are the artifact: non-JSON keyset bytes still bind, but expiry fails closed', async () => {
  const bytes = enc.encode('not json');
  const digest = await computeKeysetDigest(bytes);
  const report = structuredClone(fx.REPORT) as AttestationReport;
  report.workload_keyset_digest = digest;
  report.attestation.workload_keyset_b64 = toBase64(bytes);
  report.attestation.report_data = await computeReportData(digest, fx.NONCE);
  const result = await verifyReportBinding(report, fx.NONCE, { now: fx.NOW });
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'workload_keyset_digest').ok, true);
  assert.equal(check(result.checks, 'report_data').ok, true);
  assert.equal(check(result.checks, 'not_after').ok, false);
});
