/**
 * The high-level transcript + verifyService path, against a report captured
 * byte-exact from the reference implementation (test/fixtures/aci_report.json:
 * fixed keys, stub quote, fixed clock). The quote (id-1) is verified with
 * @phala/dcap-qvl; the stub quote does not parse, so id-1 fails closed with no
 * network. The recomputation checks (id-2/id-3) and the receipt path pass.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  jcsBytes,
  verifyReportBinding,
  reportTranscript,
  receiptTranscript,
  receiptTranscriptFromDigests,
  toHex,
  fromHex,
  toBase64,
  hashBody,
  type AttestationReport,
  type ReceiptEnvelope,
  type WorkloadKeyset,
} from '../src/index.js';
import { makeMeasuredComposeReport } from './fixtures.js';

const report = JSON.parse(
  readFileSync(new URL('../../test/fixtures/aci_report.json', import.meta.url), 'utf8'),
) as unknown as AttestationReport;

/** The nonce baked into the fixture's report_data (tests/aci_cli.rs NONCE). */
const FIXTURE_NONCE = 'cd20088d763605cf78564e5b35524ad52715419624b76e029582a3652758708d';
/** Before the fixture keyset's not_after. */
const FIXED_NOW = 1783805115;
/** Seed of the harness receipt Ed25519 key (tests/common StaticKeyProvider). */
const RECEIPT_SEED = '66'.repeat(32);
const RECEIPT_KEY_ID = 'static-receipt-ed25519';

test('transcript: quote verify (id-1) fails closed on the stub quote; bindings pass; compose is an honest skip', async () => {
  const { lines, verdict } = await reportTranscript(report, FIXTURE_NONCE, { now: FIXED_NOW });
  const byId = new Map(lines.map((l) => [l.id, l]));
  const g = (id: string) => {
    const f = byId.get(id);
    assert.ok(f, `transcript is missing ${id}`);
    return f;
  };

  for (const id of ['id-2', 'id-3']) {
    assert.equal(g(id).status, 'pass', `${id}: ${JSON.stringify(byId.get(id))}`);
  }

  // id-1 is a real quote verification now; the fixture's 47-byte stub quote
  // does not parse, so it fails closed (no PCCS fetch).
  assert.equal(g('id-1').status, 'fail');
  assert.ok((g('id-1').detail ?? '').length > 0);

  // The fixture publishes no app_compose, so the compose measurement is an
  // honest skip that names the provenance.
  assert.equal(g('id-4').status, 'skip');
  assert.ok(g('id-4').detail?.includes('deadbeef'));

  for (const id of ['id-5', 'id-6']) {
    assert.equal(g(id).status, 'skip', `${id} must be an honest skip`);
    assert.ok((g(id).detail ?? '').length > 0, `${id} needs a reason`);
  }
  assert.ok(g('id-6').detail?.includes('no live channel'));

  assert.equal(verdict.verified, false);
  assert.ok(verdict.line.startsWith('NOT VERIFIED'));
  assert.ok(verdict.line.includes('id-1'));
});

test('transcript: a wrong nonce fails the binding chain (id-2)', async () => {
  const { lines, verdict } = await reportTranscript(report, 'a'.repeat(64), { now: FIXED_NOW });
  assert.equal(verdict.verified, false);
  assert.equal(lines.find((l) => l.id === 'id-2')?.status, 'fail');
});

test('transcript: compose policy accepts reviewed releases and rejects other measurements', async () => {
  const measured = await makeMeasuredComposeReport();
  const accepted = await reportTranscript(measured.report, FIXTURE_NONCE, {
    now: FIXED_NOW,
    acceptedComposeHashes: [measured.composeHash.toUpperCase()],
  });
  assert.equal(accepted.composeHash, measured.composeHash);
  assert.equal(accepted.lines.find((line) => line.id === 'id-4')?.status, 'pass');

  const rejected = await reportTranscript(measured.report, FIXTURE_NONCE, {
    now: FIXED_NOW,
    acceptedComposeHashes: ['00'.repeat(32)],
  });
  const provenance = rejected.lines.find((line) => line.id === 'id-4');
  assert.equal(provenance?.status, 'fail');
  assert.ok(provenance?.detail?.includes(measured.composeHash));
});

test('receipt transcript: envelope verifies; a tampered payload fails receipt-1', async () => {
  const verification = await verifyReportBinding(report, FIXTURE_NONCE, { now: FIXED_NOW });
  const keyset = verification.keyset as WorkloadKeyset;
  const digest = verification.workloadKeysetDigest as string;
  assert.equal(digest, report.workload_keyset_digest);

  const requestBody = '{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}';
  const responseBody = '{"choices":[],"id":"chatcmpl-123"}';
  const unsigned = {
    api_version: 'aci/1',
    receipt_id: 'rcpt-0001',
    chat_id: 'chatcmpl-123',
    model: 'demo-model',
    workload_keyset_digest: digest,
    endpoint: '/v1/chat/completions',
    method: 'POST',
    served_at: FIXED_NOW,
    event_log: [
      { type: 'request.received', body_hash: await hashBody(requestBody) },
      { type: 'request.forwarded', body_hash: await hashBody(requestBody) },
      { type: 'response.returned', body_hash: await hashBody(responseBody) },
    ],
    key_id: RECEIPT_KEY_ID,
  };

  const privateKey = await globalThis.crypto.subtle.importKey(
    'pkcs8',
    fromHex('302e020100300506032b657004220420' + RECEIPT_SEED) as BufferSource,
    { name: 'Ed25519' },
    false,
    ['sign'],
  );
  const signature = toHex(
    new Uint8Array(
      await globalThis.crypto.subtle.sign(
        { name: 'Ed25519' },
        privateKey,
        jcsBytes(unsigned) as BufferSource,
      ),
    ),
  );
  const document = { ...unsigned, signature } as unknown as ReceiptEnvelope;

  const receipt = await receiptTranscript(document, keyset, digest, requestBody, responseBody);
  for (const id of ['receipt-1', 'receipt-2', 'receipt-3', 'receipt-4']) {
    assert.equal(receipt.lines.find((l) => l.id === id)?.status, 'pass', id);
  }
  assert.equal(receipt.verdict.verified, true);

  const streamed = await receiptTranscriptFromDigests(
    document,
    keyset,
    digest,
    { request: await hashBody(requestBody), response: await hashBody(responseBody) },
  );
  for (const id of ['receipt-1', 'receipt-2', 'receipt-3', 'receipt-4']) {
    assert.equal(streamed.lines.find((line) => line.id === id)?.status, 'pass', id);
  }

  const wrongWireHash = await receiptTranscriptFromDigests(document, keyset, digest, {
    request: await hashBody(requestBody),
    response: 'sha256:' + '00'.repeat(32),
  });
  assert.equal(wrongWireHash.lines.find((line) => line.id === 'receipt-4')?.status, 'fail');

  const tampered = { ...document, served_at: FIXED_NOW + 1 } as unknown as ReceiptEnvelope;
  const bad = await receiptTranscript(tampered, keyset, digest);
  assert.equal(bad.verdict.verified, false);
  assert.equal(bad.lines.find((l) => l.id === 'receipt-1')?.status, 'fail');

  // Appendix B: a foreign api_version must reach the verdict, not just the
  // low-level result. Re-sign so only the version is at fault.
  const foreignUnsigned = { ...unsigned, api_version: 'aci/2' };
  const foreignSignature = toHex(
    new Uint8Array(
      await globalThis.crypto.subtle.sign(
        { name: 'Ed25519' },
        privateKey,
        jcsBytes(foreignUnsigned) as BufferSource,
      ),
    ),
  );
  const foreign = {
    ...foreignUnsigned,
    signature: foreignSignature,
  } as unknown as ReceiptEnvelope;
  const foreignResult = await receiptTranscript(foreign, keyset, digest);
  assert.equal(foreignResult.lines.find((l) => l.id === 'receipt-1')?.status, 'pass');
  assert.equal(foreignResult.lines.find((l) => l.id === 'receipt-2')?.status, 'fail');
  assert.equal(foreignResult.verdict.verified, false);

  // §9.3(5): with no upstream.verified event the check does not apply — a
  // skip with its reason, never a silent pass.
  assert.equal(receipt.lines.find((l) => l.id === 'upstream-1')?.status, 'skip');
  assert.equal(receipt.lines.find((l) => l.id === 'upstream-2')?.status, 'skip');
});

test('receipt transcript: an aggregator receipt recording a failed upstream fails upstream-1', async () => {
  const verification = await verifyReportBinding(report, FIXTURE_NONCE, { now: FIXED_NOW });
  const keyset = verification.keyset as WorkloadKeyset;
  const digest = verification.workloadKeysetDigest as string;
  const privateKey = await globalThis.crypto.subtle.importKey(
    'pkcs8',
    fromHex('302e020100300506032b657004220420' + RECEIPT_SEED) as BufferSource,
    { name: 'Ed25519' },
    false,
    ['sign'],
  );
  const unsigned = {
    api_version: 'aci/1',
    receipt_id: 'rcpt-agg',
    chat_id: null,
    model: 'demo-model',
    workload_keyset_digest: digest,
    endpoint: '/v1/chat/completions',
    method: 'POST',
    served_at: FIXED_NOW,
    event_log: [
      { type: 'request.received', body_hash: 'sha256:' + '00'.repeat(32) },
      {
        type: 'upstream.verified',
        result: 'failed',
        required: true,
        reason: 'quote verification failed',
      },
      { type: 'response.returned', body_hash: 'sha256:' + '11'.repeat(32) },
    ],
    key_id: RECEIPT_KEY_ID,
  };
  const signature = toHex(
    new Uint8Array(
      await globalThis.crypto.subtle.sign(
        { name: 'Ed25519' },
        privateKey,
        jcsBytes(unsigned) as BufferSource,
      ),
    ),
  );
  const document = { ...unsigned, signature } as unknown as ReceiptEnvelope;

  const strict = await receiptTranscript(document, keyset, digest);
  assert.equal(strict.lines.find((l) => l.id === 'upstream-1')?.status, 'fail');
  assert.equal(strict.verdict.verified, false);

  // A client that does not require verified serving gets the fact, not a fail.
  const lenient = await receiptTranscript(document, keyset, digest, undefined, undefined, {
    requiresVerified: false,
  });
  assert.equal(lenient.lines.find((l) => l.id === 'upstream-1')?.status, 'info');
});
