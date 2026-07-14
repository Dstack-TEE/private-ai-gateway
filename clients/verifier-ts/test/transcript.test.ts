/**
 * The high-level transcript + verifyService path, against a report captured
 * byte-exact from the reference implementation (test/fixtures/aci_report.json:
 * fixed keys, stub quote, fixed clock). The quote (L2.1) is verified with
 * @phala/dcap-qvl; the stub quote does not parse, so L2.1 fails closed with no
 * network. The recomputation checks (L2.2/L2.3) and the receipt path pass.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import {
  verifyReportBinding,
  reportTranscript,
  receiptTranscript,
  toHex,
  fromHex,
  toBase64,
  hashBody,
  type AttestationReport,
  type ReceiptEnvelope,
  type WorkloadKeyset,
} from '../src/index.js';

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

test('transcript: quote verify (L2.1) fails closed on the stub quote; bindings pass; compose is an honest skip', async () => {
  const { lines, verdict } = await reportTranscript(report, FIXTURE_NONCE, { now: FIXED_NOW });
  const byId = new Map(lines.map((l) => [l.id, l]));
  const g = (id: string) => {
    const f = byId.get(id);
    assert.ok(f, `transcript is missing ${id}`);
    return f;
  };

  for (const id of ['L2.2', 'L2.3']) {
    assert.equal(g(id).status, 'pass', `${id}: ${JSON.stringify(byId.get(id))}`);
  }

  // L2.1 is a real quote verification now; the fixture's 47-byte stub quote
  // does not parse, so it fails closed (no PCCS fetch).
  assert.equal(g('L2.1').status, 'fail');
  assert.ok((g('L2.1').detail ?? '').length > 0);

  // The fixture publishes no app_compose, so the compose measurement is an
  // honest skip that names the provenance.
  assert.equal(g('L2.4').status, 'skip');
  assert.ok(g('L2.4').detail?.includes('deadbeef'));

  for (const id of ['L2.5', 'L2.6']) {
    assert.equal(g(id).status, 'skip', `${id} must be an honest skip`);
    assert.ok((g(id).detail ?? '').length > 0, `${id} needs a reason`);
  }
  assert.ok(g('L2.6').detail?.includes('WebPKI'));

  assert.equal(verdict.verified, false);
  assert.ok(verdict.line.startsWith('NOT VERIFIED'));
  assert.ok(verdict.line.includes('L2.1'));
});

test('transcript: a wrong nonce fails the binding chain (L2.2)', async () => {
  const { lines, verdict } = await reportTranscript(report, 'a'.repeat(64), { now: FIXED_NOW });
  assert.equal(verdict.verified, false);
  assert.equal(lines.find((l) => l.id === 'L2.2')?.status, 'fail');
});

test('receipt transcript: envelope verifies; a tampered payload fails R.1', async () => {
  const verification = await verifyReportBinding(report, FIXTURE_NONCE, { now: FIXED_NOW });
  const keyset = verification.keyset as WorkloadKeyset;
  const digest = verification.workloadKeysetDigest as string;
  assert.equal(digest, report.workload_keyset_digest);

  const requestBody = '{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}';
  const responseBody = '{"choices":[],"id":"chatcmpl-123"}';
  const payloadBytes = new TextEncoder().encode(
    JSON.stringify({
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
    }),
  );

  const privateKey = await globalThis.crypto.subtle.importKey(
    'pkcs8',
    fromHex('302e020100300506032b657004220420' + RECEIPT_SEED) as BufferSource,
    { name: 'Ed25519' },
    false,
    ['sign'],
  );
  const signature = toHex(
    new Uint8Array(
      await globalThis.crypto.subtle.sign({ name: 'Ed25519' }, privateKey, payloadBytes as BufferSource),
    ),
  );
  const envelope: ReceiptEnvelope = {
    payload_b64: toBase64(payloadBytes),
    key_id: RECEIPT_KEY_ID,
    algo: 'ed25519',
    signature,
  };

  const receipt = await receiptTranscript(envelope, keyset, digest, requestBody, responseBody);
  for (const id of ['R.1', 'R.2', 'R.3', 'R.4']) {
    assert.equal(receipt.lines.find((l) => l.id === id)?.status, 'pass', id);
  }
  assert.equal(receipt.verdict.verified, true);

  const tampered: ReceiptEnvelope = { ...envelope, payload_b64: toBase64(payloadBytes.slice(1)) };
  const bad = await receiptTranscript(tampered, keyset, digest);
  assert.equal(bad.verdict.verified, false);
  assert.equal(bad.lines.find((l) => l.id === 'R.1')?.status, 'fail');
});
