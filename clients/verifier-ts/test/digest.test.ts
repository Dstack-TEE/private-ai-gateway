import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  attestationStatement,
  computeReportData,
  computeKeysetDigest,
  computeSessionId,
  checkSessionApiVersion,
  checkSessionEvidence,
  sha256Hex,
  sha256Prefixed,
  AciFormatError,
} from '../src/index.js';
import * as fx from './fixtures.js';

const enc = new TextEncoder();
const dec = new TextDecoder();

test('§4.2 attestation statement: exact bytes, fixed member order, no whitespace', () => {
  const statement = attestationStatement(fx.KEYSET_DIGEST, 'test-nonce');
  assert.equal(
    dec.decode(statement),
    `{"keyset_digest":"${fx.KEYSET_DIGEST}","nonce":"test-nonce","purpose":"aci.report_data.v1"}`,
  );
});

test('§4.2 omitted nonce is the JSON literal null, without quotes', () => {
  const expected = `{"keyset_digest":"${fx.KEYSET_DIGEST}","nonce":null,"purpose":"aci.report_data.v1"}`;
  assert.equal(dec.decode(attestationStatement(fx.KEYSET_DIGEST, null)), expected);
  assert.equal(dec.decode(attestationStatement(fx.KEYSET_DIGEST, undefined)), expected);
});

test('§4.2 the template rejects inputs that would need escaping', () => {
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, 'bad nonce'), AciFormatError);
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, '"quoted"'), AciFormatError);
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, ''), AciFormatError);
  assert.throws(() => attestationStatement(fx.KEYSET_DIGEST, 'a'.repeat(129)), AciFormatError);
  assert.throws(() => attestationStatement('not-a-digest', 'ok'), AciFormatError);
  assert.throws(() => attestationStatement('sha256:' + 'A'.repeat(64), 'ok'), AciFormatError);
});

test('§4.2 report_data is the bare-hex SHA-256 of the statement bytes', async () => {
  const expected = await sha256Hex(attestationStatement(fx.KEYSET_DIGEST, fx.NONCE));
  assert.equal(await computeReportData(fx.KEYSET_DIGEST, fx.NONCE), expected);
  assert.ok(!expected.startsWith('sha256:'));
  assert.equal(expected.length, 64);
});

test('§4.1 keyset digest and §9 session id hash the exact served bytes', async () => {
  assert.equal(await computeKeysetDigest(fx.KEYSET_BYTES), await sha256Prefixed(fx.KEYSET_BYTES));
  assert.equal(await computeSessionId(fx.SESSION_BYTES), await sha256Prefixed(fx.SESSION_BYTES));
  // One changed byte is a different artifact.
  const tampered = enc.encode(dec.decode(fx.SESSION_BYTES) + ' ');
  assert.notEqual(await computeSessionId(tampered), fx.SESSION_ID);
});

test('§10.3(4) session evidence data URI decodes and hashes to its digest', async () => {
  assert.equal(await checkSessionEvidence(fx.SESSION.evidence), true);

  const wrongDigest = { ...fx.SESSION.evidence, digest: 'sha256:' + '00'.repeat(32) };
  assert.equal(await checkSessionEvidence(wrongDigest), false);

  const notDataUri = { ...fx.SESSION.evidence, data: 'https://example.com/evidence' };
  assert.equal(await checkSessionEvidence(notDataUri), false);

  const noData = { digest: fx.SESSION.evidence.digest };
  assert.equal(await checkSessionEvidence(noData), false);
});

test('Appendix A: session documents with a foreign api_version are rejected', () => {
  assert.equal(checkSessionApiVersion(fx.SESSION), true);
  assert.equal(checkSessionApiVersion({ ...fx.SESSION, api_version: 'aci/2' }), false);
});
