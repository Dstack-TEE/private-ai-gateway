import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  computeWorkloadId,
  computeKeysetDigest,
  attestationStatement,
  computeReportData,
  canonicalize,
  receiptSigningBytes,
  computeSessionId,
  sessionMaterial,
  sha256Prefixed,
} from '../src/index.js';
import * as fx from './fixtures.js';

const enc = (s: string) => new TextEncoder().encode(s);

test('§1 workload_id = sha256(JCS(public_key))', async () => {
  assert.equal(await computeWorkloadId(fx.IDENTITY_PUBLIC_KEY_OBJ), fx.WORKLOAD_ID);
});

test('§1 workload_id JCS byte string matches the vector', () => {
  assert.equal(
    canonicalize({ algo: 'ed25519', public_key: fx.IDENTITY_PUBLIC_KEY }),
    `{"algo":"ed25519","public_key":"${fx.IDENTITY_PUBLIC_KEY}"}`,
  );
});

test('§2 workload_keyset_digest = sha256(JCS(keyset))', async () => {
  assert.equal(await computeKeysetDigest(fx.KEYSET), fx.KEYSET_DIGEST);
});

test('§3 attestation statement JCS matches the vector', () => {
  assert.equal(
    canonicalize(attestationStatement(fx.WORKLOAD_ID, fx.KEYSET_DIGEST, fx.ATTESTATION_NONCE)),
    `{"nonce":"test-nonce","purpose":"aci.report_data.v1","workload_id":"${fx.WORKLOAD_ID}","workload_keyset_digest":"${fx.KEYSET_DIGEST}"}`,
  );
});

test('§3 report_data (nonce present)', async () => {
  assert.equal(
    await computeReportData(fx.WORKLOAD_ID, fx.KEYSET_DIGEST, fx.ATTESTATION_NONCE),
    fx.REPORT_DATA_WITH_NONCE,
  );
});

test('§3 report_data (nonce omitted → null, not the string "null")', async () => {
  assert.equal(
    await computeReportData(fx.WORKLOAD_ID, fx.KEYSET_DIGEST, undefined),
    fx.REPORT_DATA_NONCE_OMITTED,
  );
  assert.equal(
    await computeReportData(fx.WORKLOAD_ID, fx.KEYSET_DIGEST, null),
    fx.REPORT_DATA_NONCE_OMITTED,
  );
});

test('§5 evidence.digest = sha256(evidence bytes)', async () => {
  assert.equal(await sha256Prefixed(enc(fx.EVIDENCE_BYTES)), fx.EVIDENCE_DIGEST);
});

test('§5 session material JCS matches the vector (identity restored to null)', () => {
  assert.equal(
    canonicalize(sessionMaterial(fx.SESSION_RECORD)),
    `{"channel_binding":[{"origin":"https://upstream.example.com","spki_sha256":"${'d1'.repeat(32)}","type":"tls_spki_sha256"}],"claims":{"gpu_attested":{"status":"unknown"},"model_weights_provenance":{"status":"unknown"},"os_known_good":{"status":"unknown"},"serving_software_known_good":{"status":"unknown"},"tcb_up_to_date":{"status":"unknown"},"tee_attested":{"reason":"example quote verified","source":"hardware_proven","status":"asserted"}},"endpoint":"https://upstream.example.com","evidence_digest":"${fx.EVIDENCE_DIGEST}","identity":null,"upstream_name":"demo-upstream","verifier_id":"example/1"}`,
  );
});

test('§5 session_id = as_ + sha256(JCS(material))', async () => {
  assert.equal(await computeSessionId(fx.SESSION_RECORD), fx.SESSION_ID);
});

test('§6 request/response body hashes', async () => {
  assert.equal(await sha256Prefixed(enc(fx.REQUEST_BODY)), fx.REQUEST_BODY_HASH);
  assert.equal(await sha256Prefixed(enc(fx.RESPONSE_BODY)), fx.RESPONSE_BODY_HASH);
});

test('§6 receipt canonical signing bytes match the vector and hash', async () => {
  const canonical = new TextDecoder().decode(receiptSigningBytes(fx.RECEIPT));
  // signature.value is dropped; algo and key_id stay.
  assert.equal(canonical.includes('"signature":{"algo":"ed25519","key_id":"receipt-1"}'), true);
  assert.equal(canonical.includes(fx.RECEIPT_SIGNATURE), false);
  assert.equal(await sha256Prefixed(receiptSigningBytes(fx.RECEIPT)), `sha256:${fx.RECEIPT_CANONICAL_SHA256}`);
});
