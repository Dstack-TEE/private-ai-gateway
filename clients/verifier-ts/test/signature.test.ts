import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  verifyEd25519,
  verifySignature,
  fromHex,
  keysetEndorsementPayload,
  keysetRevocationPayload,
  receiptSigningBytes,
  UnsupportedAlgorithmError,
} from '../src/index.js';
import * as fx from './fixtures.js';

test('the documented seeds derive the published public keys', async () => {
  const id = await fx.ed25519FromSeed(fx.IDENTITY_SEED);
  const rcpt = await fx.ed25519FromSeed(fx.RECEIPT_SEED);
  assert.equal(id.publicKeyHex, fx.IDENTITY_PUBLIC_KEY);
  assert.equal(rcpt.publicKeyHex, fx.RECEIPT_PUBLIC_KEY);
});

test('§4.3 keyset endorsement: signature verifies and re-signs to the vector', async () => {
  const payload = keysetEndorsementPayload(fx.KEYSET_DIGEST);
  const ok = await verifyEd25519(fromHex(fx.IDENTITY_PUBLIC_KEY), fromHex(fx.ENDORSEMENT_SIGNATURE), payload);
  assert.equal(ok, true);
  // Ed25519 is deterministic: re-signing reproduces the vector byte-for-byte.
  const id = await fx.ed25519FromSeed(fx.IDENTITY_SEED);
  assert.equal(await fx.ed25519SignHex(id.privateKey, payload), fx.ENDORSEMENT_SIGNATURE);
});

test('§4.7 keyset revocation: signature verifies and re-signs to the vector', async () => {
  const payload = keysetRevocationPayload(fx.KEYSET_DIGEST);
  const ok = await verifyEd25519(fromHex(fx.IDENTITY_PUBLIC_KEY), fromHex(fx.REVOCATION_SIGNATURE), payload);
  assert.equal(ok, true);
  const id = await fx.ed25519FromSeed(fx.IDENTITY_SEED);
  assert.equal(await fx.ed25519SignHex(id.privateKey, payload), fx.REVOCATION_SIGNATURE);
});

test('§8.5 receipt signature verifies and re-signs to the vector', async () => {
  const canonical = receiptSigningBytes(fx.RECEIPT);
  const ok = await verifyEd25519(fromHex(fx.RECEIPT_PUBLIC_KEY), fromHex(fx.RECEIPT_SIGNATURE), canonical);
  assert.equal(ok, true);
  const rcpt = await fx.ed25519FromSeed(fx.RECEIPT_SEED);
  assert.equal(await fx.ed25519SignHex(rcpt.privateKey, canonical), fx.RECEIPT_SIGNATURE);
});

test('a tampered payload fails verification', async () => {
  const payload = keysetEndorsementPayload(fx.KEYSET_DIGEST);
  const tampered = new Uint8Array(payload);
  tampered[0] = (tampered[0] ?? 0) ^ 0x01;
  assert.equal(
    await verifyEd25519(fromHex(fx.IDENTITY_PUBLIC_KEY), fromHex(fx.ENDORSEMENT_SIGNATURE), tampered),
    false,
  );
});

test('a signature under the wrong key fails verification', async () => {
  const payload = keysetEndorsementPayload(fx.KEYSET_DIGEST);
  assert.equal(
    await verifyEd25519(fromHex(fx.RECEIPT_PUBLIC_KEY), fromHex(fx.ENDORSEMENT_SIGNATURE), payload),
    false,
  );
});

test('secp256k1 is out of scope: verifySignature throws UnsupportedAlgorithmError', async () => {
  await assert.rejects(
    () =>
      verifySignature(
        'ecdsa-secp256k1',
        fromHex(fx.RECEIPT_PUBLIC_KEY),
        fromHex(fx.RECEIPT_SIGNATURE),
        keysetEndorsementPayload(fx.KEYSET_DIGEST),
        'test',
      ),
    UnsupportedAlgorithmError,
  );
});
