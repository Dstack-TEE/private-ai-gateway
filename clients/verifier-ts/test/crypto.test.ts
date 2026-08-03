import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  toHex,
  fromHex,
  toBase64,
  fromBase64,
  sha256Hex,
  sha256Prefixed,
  verifyEd25519,
  AciFormatError,
} from '../src/index.js';
import * as fx from './fixtures.js';

const enc = new TextEncoder();

test('hex round trip, 0x prefix, and malformed input', () => {
  const bytes = new Uint8Array([0, 1, 0xab, 0xff]);
  assert.equal(toHex(bytes), '0001abff');
  assert.deepEqual(fromHex('0001abff'), bytes);
  assert.deepEqual(fromHex('0x0001abff'), bytes);
  assert.throws(() => fromHex('abc'), AciFormatError);
  assert.throws(() => fromHex('zz'), AciFormatError);
});

test('base64 round trip over all byte values, and malformed input', () => {
  const bytes = new Uint8Array(256);
  for (let i = 0; i < 256; i++) bytes[i] = i;
  assert.deepEqual(fromBase64(toBase64(bytes)), bytes);
  assert.equal(toBase64(new Uint8Array(0)), '');
  assert.throws(() => fromBase64('!not base64!'), AciFormatError);
});

test('sha256 matches the published NIST vector for "abc"', async () => {
  const expected = 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad';
  assert.equal(await sha256Hex(enc.encode('abc')), expected);
  assert.equal(await sha256Prefixed(enc.encode('abc')), 'sha256:' + expected);
});

test('verifyEd25519 accepts a valid signature and rejects tampering', async () => {
  const message = enc.encode('attested bytes');
  const signature = fromHex(await fx.ed25519SignHex(fx.receiptKey.privateKey, message));
  const publicKey = fromHex(fx.receiptKey.publicKeyHex);

  assert.equal(await verifyEd25519(publicKey, signature, message), true);

  const badSig = signature.slice();
  badSig[0] = badSig[0]! ^ 0xff;
  assert.equal(await verifyEd25519(publicKey, badSig, message), false);
  assert.equal(await verifyEd25519(publicKey, signature, enc.encode('other bytes')), false);
  // A malformed key is a failed verification, never a throw.
  assert.equal(await verifyEd25519(new Uint8Array(3), signature, message), false);
});
