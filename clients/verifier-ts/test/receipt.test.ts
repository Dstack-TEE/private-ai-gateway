import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  verifyReceipt,
  findEvent,
  hashBody,
  checkRequestBodyHash,
  checkResponseWireHash,
  checkResponseCleartextHash,
  UnsupportedAlgorithmError,
  type Receipt,
  type WorkloadKeyset,
} from '../src/index.js';
import * as fx from './fixtures.js';

test('§10.2 verifyReceipt: valid receipt passes every check', async () => {
  const result = await verifyReceipt(fx.RECEIPT, fx.KEYSET);
  assert.equal(result.ok, true, JSON.stringify(result.checks));
  assert.deepEqual(
    result.checks.map((c) => c.name).sort(),
    ['signature', 'workload_id', 'workload_keyset_digest'],
  );
});

test('§10.2 check 3/4: body-hash helpers match the receipt events', async () => {
  assert.equal(await hashBody(fx.REQUEST_BODY), fx.REQUEST_BODY_HASH);
  assert.equal(await checkRequestBodyHash(fx.RECEIPT, fx.REQUEST_BODY), true);
  assert.equal(await checkResponseWireHash(fx.RECEIPT, fx.RESPONSE_BODY), true);
  // Plaintext case: cleartext_hash equals wire_hash.
  assert.equal(await checkResponseCleartextHash(fx.RECEIPT, fx.RESPONSE_BODY), true);
});

test('body-hash helpers reject the wrong bytes', async () => {
  assert.equal(await checkRequestBodyHash(fx.RECEIPT, fx.REQUEST_BODY + ' '), false);
  assert.equal(await checkResponseWireHash(fx.RECEIPT, '{"choices":[]}'), false);
});

test('findEvent locates events by type', () => {
  assert.equal(findEvent(fx.RECEIPT, 'request.received')?.seq, 0);
  assert.equal(findEvent(fx.RECEIPT, 'upstream.verified')?.seq, 2);
  assert.equal(findEvent(fx.RECEIPT, 'nope'), undefined);
});

test('verifyReceipt fails on a tampered signature', async () => {
  const bad = structuredClone(fx.RECEIPT) as Receipt;
  bad.signature.value = bad.signature.value.replace(/^../, '00');
  const result = await verifyReceipt(bad, fx.KEYSET);
  assert.equal(result.ok, false);
  assert.equal(result.checks.find((c) => c.name === 'signature')?.ok, false);
});

test('verifyReceipt fails on a mismatched workload_id', async () => {
  const bad = structuredClone(fx.RECEIPT) as Receipt;
  bad.workload_id = 'sha256:' + '00'.repeat(32);
  const result = await verifyReceipt(bad, fx.KEYSET);
  assert.equal(result.ok, false);
  assert.equal(result.checks.find((c) => c.name === 'workload_id')?.ok, false);
});

test('verifyReceipt fails when key_id is not in the keyset', async () => {
  const bad = structuredClone(fx.RECEIPT) as Receipt;
  bad.signature.key_id = 'unknown-key';
  const result = await verifyReceipt(bad, fx.KEYSET);
  assert.equal(result.ok, false);
  const sig = result.checks.find((c) => c.name === 'signature');
  assert.equal(sig?.ok, false);
  assert.equal(sig?.detail?.includes('unknown-key'), true);
});

test('verifyReceipt fails when signature.algo disagrees with the keyset entry', async () => {
  const bad = structuredClone(fx.RECEIPT) as Receipt;
  bad.signature.algo = 'ecdsa-secp256k1';
  const result = await verifyReceipt(bad, fx.KEYSET);
  assert.equal(result.ok, false);
  assert.equal(result.checks.find((c) => c.name === 'signature')?.ok, false);
});

test('verifyReceipt throws when the attested key uses secp256k1', async () => {
  const keyset = structuredClone(fx.KEYSET) as WorkloadKeyset;
  keyset.receipt_signing_keys[0]!.algo = 'ecdsa-secp256k1';
  const receipt = structuredClone(fx.RECEIPT) as Receipt;
  receipt.signature.algo = 'ecdsa-secp256k1';
  await assert.rejects(() => verifyReceipt(receipt, keyset), UnsupportedAlgorithmError);
});
