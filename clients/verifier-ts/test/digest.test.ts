import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  jcsBytes,
  attestationStatement,
  computeSessionId,
  checkSessionApiVersion,
  checkSessionEvidence,
  sha256Hex,
  AciFormatError,
} from '../src/index.js';
import * as fx from './fixtures.js';

const enc = new TextEncoder();
const dec = new TextDecoder();

test('§3.2 attestation statement: exact bytes, fixed member order, no whitespace', () => {
  const statement = attestationStatement(fx.KEYSET_DIGEST, '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f');
  assert.equal(
    dec.decode(statement),
    `{"keyset_digest":"${fx.KEYSET_DIGEST}","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.report_data.v1"}`,
  );
});

test('§3.2 omitted nonce is the JSON literal null, without quotes', () => {
  const expected = `{"keyset_digest":"${fx.KEYSET_DIGEST}","nonce":null,"purpose":"aci.report_data.v1"}`;
  assert.equal(dec.decode(attestationStatement(fx.KEYSET_DIGEST, null)), expected);
  assert.equal(dec.decode(attestationStatement(fx.KEYSET_DIGEST, undefined)), expected);
});

test('§3.2 the nonce rule is exactly 64 lowercase hex, nothing looser', () => {
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, 'bad nonce'), AciFormatError);
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, ''), AciFormatError);
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, 'a'.repeat(63)), AciFormatError);
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, 'a'.repeat(65)), AciFormatError);
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, 'A'.repeat(64)), AciFormatError);
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, 'g'.repeat(64)), AciFormatError);
  assert.throws(() => attestationStatement('not-a-digest', 'a'.repeat(64)), AciFormatError);
  assert.throws(
    () => attestationStatement('sha256:' + 'A'.repeat(64), 'a'.repeat(64)),
    AciFormatError,
  );
});

test('§8 session id: the served encoding is free, the content is not', async () => {
  // The encoding is free: a re-parsed copy yields the same id.
  assert.equal(await computeSessionId(JSON.parse(dec.decode(fx.SESSION_BYTES))), fx.SESSION_ID);
  // A changed member is a different artifact.
  assert.notEqual(await computeSessionId({ ...fx.SESSION, verifier_id: 'other/1' }), fx.SESSION_ID);
});

test('§9.3(4) session evidence data URI decodes and hashes to its digest', async () => {
  assert.equal(await checkSessionEvidence(fx.SESSION.evidence), true);

  const wrongDigest = { ...fx.SESSION.evidence, digest: 'sha256:' + '00'.repeat(32) };
  assert.equal(await checkSessionEvidence(wrongDigest), false);

  const notDataUri = { ...fx.SESSION.evidence, data: 'https://example.com/evidence' };
  assert.equal(await checkSessionEvidence(notDataUri), false);

  const noData = { digest: fx.SESSION.evidence.digest };
  assert.equal(await checkSessionEvidence(noData), false);
});

test('Appendix B: session documents with a foreign api_version are rejected', () => {
  assert.equal(checkSessionApiVersion(fx.SESSION), true);
  assert.equal(checkSessionApiVersion({ ...fx.SESSION, api_version: 'aci/2' }), false);
});
