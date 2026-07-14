import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  verifyReceipt,
  findEvent,
  hashBody,
  checkRequestBodyHash,
  checkResponseBodyHash,
  toBase64,
  type ReceiptEnvelope,
  type WorkloadKeyset,
  type Check,
} from '../src/index.js';
import * as fx from './fixtures.js';

const enc = new TextEncoder();

function check(checks: Check[], name: string): Check {
  const found = checks.find((c) => c.name === name);
  assert.ok(found, `missing check "${name}"`);
  return found;
}

test('§10.2 checks 1–2: a valid envelope verifies and yields the parsed payload', async () => {
  const result = await verifyReceipt(fx.ENVELOPE, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(result.ok, true, JSON.stringify(result.checks));
  assert.deepEqual(
    result.checks.map((c) => c.name),
    ['signature', 'api_version', 'workload_keyset_digest'],
  );
  assert.equal(result.payload?.receipt_id, 'rcpt-0001');
  assert.equal(result.payload?.model, 'demo-model');
});

test('a tampered signature fails check 1', async () => {
  const bad = structuredClone(fx.ENVELOPE) as ReceiptEnvelope;
  bad.signature = bad.signature.replace(/^../, bad.signature.startsWith('00') ? '11' : '00');
  const result = await verifyReceipt(bad, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'signature').ok, false);
});

test('tampered payload bytes fail check 1 — the signature covers the served bytes', async () => {
  const tampered = structuredClone(fx.RECEIPT_PAYLOAD);
  tampered.served_at = 1750000001;
  const bad = structuredClone(fx.ENVELOPE) as ReceiptEnvelope;
  bad.payload_b64 = toBase64(enc.encode(JSON.stringify(tampered)));
  const result = await verifyReceipt(bad, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'signature').ok, false);
  assert.equal(check(result.checks, 'workload_keyset_digest').ok, true);
});

test('an unknown key_id fails check 1', async () => {
  const bad = structuredClone(fx.ENVELOPE) as ReceiptEnvelope;
  bad.key_id = 'receipt-99';
  const result = await verifyReceipt(bad, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(check(result.checks, 'signature').ok, false);
  assert.ok(check(result.checks, 'signature').detail?.includes('receipt-99'));
});

test('the attested keyset entry decides the algorithm; the envelope may not override it', async () => {
  const bad = structuredClone(fx.ENVELOPE) as ReceiptEnvelope;
  bad.algo = 'ecdsa-secp256k1';
  const result = await verifyReceipt(bad, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(check(result.checks, 'signature').ok, false);
});

test('a non-ed25519 keyset entry is rejected, not silently skipped (Appendix A)', async () => {
  const keyset = structuredClone(fx.KEYSET) as WorkloadKeyset;
  keyset.receipt_signing_keys[0]!.algo = 'frobnitz';
  const envelope = structuredClone(fx.ENVELOPE) as ReceiptEnvelope;
  envelope.algo = 'frobnitz';
  const result = await verifyReceipt(envelope, keyset, fx.KEYSET_DIGEST);
  assert.equal(check(result.checks, 'signature').ok, false);
  assert.ok(check(result.checks, 'signature').detail?.includes('unsupported'));
});

test('a payload with a foreign api_version is rejected (Appendix A)', async () => {
  const payload = structuredClone(fx.RECEIPT_PAYLOAD);
  payload.api_version = 'aci/2';
  const envelope = await fx.makeEnvelope(enc.encode(JSON.stringify(payload)));
  const result = await verifyReceipt(envelope, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'signature').ok, true);
  assert.equal(check(result.checks, 'api_version').ok, false);
});

test('a payload bound to a different keyset digest fails check 2', async () => {
  const result = await verifyReceipt(fx.ENVELOPE, fx.KEYSET, 'sha256:' + '00'.repeat(32));
  assert.equal(result.ok, false);
  assert.equal(check(result.checks, 'signature').ok, true);
  assert.equal(check(result.checks, 'workload_keyset_digest').ok, false);
});

test('undecodable payload_b64 fails both checks', async () => {
  const bad = structuredClone(fx.ENVELOPE) as ReceiptEnvelope;
  bad.payload_b64 = '!not base64!';
  const result = await verifyReceipt(bad, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(result.ok, false);
  assert.equal(result.payload, undefined);
  assert.equal(check(result.checks, 'signature').ok, false);
  assert.equal(check(result.checks, 'workload_keyset_digest').ok, false);
});

test('a signed non-JSON payload verifies its signature but cannot bind', async () => {
  const envelope = await fx.makeEnvelope(enc.encode('not json'));
  const result = await verifyReceipt(envelope, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(result.ok, false);
  assert.equal(result.payload, undefined);
  assert.equal(check(result.checks, 'signature').ok, true);
  assert.equal(check(result.checks, 'workload_keyset_digest').ok, false);
});

test('unknown extension event types are ignored (§8.4)', async () => {
  const payload = structuredClone(fx.RECEIPT_PAYLOAD);
  payload.event_log.push({ type: 'x.routing.decision', route: 'demo' });
  const envelope = await fx.makeEnvelope(enc.encode(JSON.stringify(payload)));
  const result = await verifyReceipt(envelope, fx.KEYSET, fx.KEYSET_DIGEST);
  assert.equal(result.ok, true);
});

test('§10.2 checks 3–4: body-hash helpers match the receipt events', async () => {
  assert.ok((await hashBody(fx.REQUEST_BODY)).startsWith('sha256:'));
  assert.equal(await checkRequestBodyHash(fx.RECEIPT_PAYLOAD, fx.REQUEST_BODY), true);
  assert.equal(await checkResponseBodyHash(fx.RECEIPT_PAYLOAD, fx.RESPONSE_BODY), true);
  assert.equal(await checkRequestBodyHash(fx.RECEIPT_PAYLOAD, fx.REQUEST_BODY + ' '), false);
  assert.equal(await checkResponseBodyHash(fx.RECEIPT_PAYLOAD, '{"choices":[]}'), false);
});

test('findEvent locates events by type; a missing event fails the hash checks', async () => {
  assert.equal(findEvent(fx.RECEIPT_PAYLOAD, 'upstream.verified')?.session_id, fx.SESSION_ID);
  assert.equal(findEvent(fx.RECEIPT_PAYLOAD, 'nope'), undefined);
  const noEvents = { ...fx.RECEIPT_PAYLOAD, event_log: [] };
  assert.equal(await checkRequestBodyHash(noEvents, fx.REQUEST_BODY), false);
  assert.equal(await checkResponseBodyHash(noEvents, fx.RESPONSE_BODY), false);
});
