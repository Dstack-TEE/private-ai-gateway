/**
 * Byte-exact pins against spec/test-vectors.md — the cross-implementation
 * check. Every digest, signature, AAD, and sealed unit published there is
 * recomputed here from the vector inputs; the sealing vectors pin the
 * ephemeral keys and GCM nonces a real client draws fresh per unit (§7.1).
 * The constants are copied verbatim from the spec document.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  toHex,
  fromHex,
  toBase64,
  fromBase64,
  sha256Hex,
  verifyEd25519,
  computeKeysetDigest,
  attestationStatement,
  computeReportData,
  computeSessionId,
  checkSessionEvidence,
  verifyReportBinding,
  verifyReceipt,
  findEvent,
  hashBody,
  checkRequestBodyHash,
  checkResponseBodyHash,
  e2eeAad,
  openUnit,
  REQUEST_CONTEXT,
  RESPONSE_CONTEXT,
  type E2eeContext,
  type ReceiptEnvelope,
  type SessionRecord,
  type WorkloadKeyset,
} from '../src/index.js';
import * as fx from './fixtures.js';

const subtle = globalThis.crypto.subtle;
const enc = new TextEncoder();
const dec = new TextDecoder();

// --- Constants from spec/test-vectors.md, verbatim -------------------------------

const KEYSET_JSON = "{\"subject\":\"dstack-app://example-app\",\"not_after\":1800000000,\"receipt_signing_keys\":[{\"key_id\":\"receipt-1\",\"algo\":\"ed25519\",\"public_key\":\"8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394\"}],\"e2ee_public_keys\":[{\"key_id\":\"e2ee-1\",\"algo\":\"x25519-aes-256-gcm-hkdf-sha256\",\"public_key\":\"5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22\"}],\"tls_public_keys\":[{\"spki_sha256\":\"c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0\",\"domain\":\"api.example.com\"}]}";
const KEYSET_B64 = "eyJzdWJqZWN0IjoiZHN0YWNrLWFwcDovL2V4YW1wbGUtYXBwIiwibm90X2FmdGVyIjoxODAwMDAwMDAwLCJyZWNlaXB0X3NpZ25pbmdfa2V5cyI6W3sia2V5X2lkIjoicmVjZWlwdC0xIiwiYWxnbyI6ImVkMjU1MTkiLCJwdWJsaWNfa2V5IjoiODEzOTc3MGVhODdkMTc1ZjU2YTM1NDY2YzM0YzdlY2NjYjhkOGE5MWI0ZWUzN2EyNWRmNjBmNWI4ZmM5YjM5NCJ9XSwiZTJlZV9wdWJsaWNfa2V5cyI6W3sia2V5X2lkIjoiZTJlZS0xIiwiYWxnbyI6IngyNTUxOS1hZXMtMjU2LWdjbS1oa2RmLXNoYTI1NiIsInB1YmxpY19rZXkiOiI1ZGZlZGQzYjZiZDQ3ZjZmYTI4ZWUxNWQ5NjlkNWJiMGVhNTM3NzRkNDg4YmRhZjlkZjFjNmUwMTI0YjNlZjIyIn1dLCJ0bHNfcHVibGljX2tleXMiOlt7InNwa2lfc2hhMjU2IjoiYzBjMGMwYzBjMGMwYzBjMGMwYzBjMGMwYzBjMGMwYzBjMGMwYzBjMGMwYzBjMGMwYzBjMGMwYzBjMGMwYzBjMCIsImRvbWFpbiI6ImFwaS5leGFtcGxlLmNvbSJ9XX0=";
const KEYSET_DIGEST = "sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4";
const STATEMENT_WITH_NONCE = "{\"keyset_digest\":\"sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4\",\"nonce\":\"test-nonce\",\"purpose\":\"aci.report_data.v1\"}";
const REPORT_DATA_WITH_NONCE = "8b899aae55437dec4d1d0d435920e112aca2a74d17595eeb601a7764d901ea07";
const STATEMENT_NULL_NONCE = "{\"keyset_digest\":\"sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4\",\"nonce\":null,\"purpose\":\"aci.report_data.v1\"}";
const REPORT_DATA_NULL_NONCE = "a98b0e34ef2ce05cf7d3fd64d86889deaf6836b8aa4e5d8baa9dd437fea07987";
const REPORT_DATA_SLOT = "8b899aae55437dec4d1d0d435920e112aca2a74d17595eeb601a7764d901ea070000000000000000000000000000000000000000000000000000000000000000";
const EVIDENCE_DIGEST = "sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d";
const EVIDENCE_DATA = "data:text/plain;base64,ZXhhbXBsZS1ldmlkZW5jZQ==";
const SESSION_JSON = "{\"api_version\":\"aci/1\",\"upstream_name\":\"demo-upstream\",\"endpoint\":\"https://upstream.example.com\",\"verifier_id\":\"example/1\",\"established_at\":1750000000,\"expires_at\":1750003600,\"channel_binding\":[{\"type\":\"tls_spki_sha256\",\"origin\":\"https://upstream.example.com\",\"spki_sha256\":\"d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1\"}],\"claims\":{\"tee_attested\":{\"status\":\"asserted\",\"source\":\"hardware_proven\",\"reason\":\"example quote verified\"},\"gpu_attested\":{\"status\":\"unknown\"},\"tcb_up_to_date\":{\"status\":\"unknown\"},\"os_known_good\":{\"status\":\"unknown\"},\"serving_software_known_good\":{\"status\":\"unknown\"},\"model_weights_provenance\":{\"status\":\"unknown\"},\"extra\":{\"gpu_arch\":\"HOPPER\",\"tcb_status\":\"UpToDate\"}},\"evidence\":{\"digest\":\"sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d\",\"data\":\"data:text/plain;base64,ZXhhbXBsZS1ldmlkZW5jZQ==\"}}";
const SESSION_ID = "sha256:a595d269728e15fe8236af46586fe84f220696c0d7d4e647eed36922b7b20cb6";
const REQUEST_BODY = "{\"messages\":[{\"content\":\"hi\",\"role\":\"user\"}],\"model\":\"demo-model\"}";
const REQUEST_BODY_HASH = "sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b";
const RESPONSE_BODY = "{\"choices\":[],\"id\":\"chatcmpl-123\"}";
const RESPONSE_BODY_HASH = "sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61";
const PAYLOAD_JSON = "{\"api_version\":\"aci/1\",\"receipt_id\":\"rcpt-0001\",\"chat_id\":\"chatcmpl-123\",\"model\":\"demo-model\",\"workload_keyset_digest\":\"sha256:1319a457f6abf587cd9c823bce5f467cedbde84c1b1ed9fef53c9cf0a3c2f1f4\",\"endpoint\":\"/v1/chat/completions\",\"method\":\"POST\",\"served_at\":1750000000,\"event_log\":[{\"type\":\"request.received\",\"body_hash\":\"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b\"},{\"type\":\"request.forwarded\",\"body_hash\":\"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b\"},{\"type\":\"upstream.verified\",\"result\":\"verified\",\"required\":true,\"model_id\":\"demo-model\",\"session_id\":\"sha256:a595d269728e15fe8236af46586fe84f220696c0d7d4e647eed36922b7b20cb6\"},{\"type\":\"response.returned\",\"body_hash\":\"sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61\"}]}";
const PAYLOAD_SHA256 = "5a04d7ce350a09a9faa4f32e5a21790cd1080a46239039538bac98c798dc2dab";
const SIGNATURE = "b0b2c830be73d6b6ad9a90b75b9c347a930e6a918e6e4f70ad1c3ce0d3dbfe6789504be5f7d317d24ba9eb84cd8bf634d58e898de89baa7fc939abd12e1b7400";
const ENVELOPE_JSON = "{\"payload_b64\":\"eyJhcGlfdmVyc2lvbiI6ImFjaS8xIiwicmVjZWlwdF9pZCI6InJjcHQtMDAwMSIsImNoYXRfaWQiOiJjaGF0Y21wbC0xMjMiLCJtb2RlbCI6ImRlbW8tbW9kZWwiLCJ3b3JrbG9hZF9rZXlzZXRfZGlnZXN0Ijoic2hhMjU2OjEzMTlhNDU3ZjZhYmY1ODdjZDljODIzYmNlNWY0NjdjZWRiZGU4NGMxYjFlZDlmZWY1M2M5Y2YwYTNjMmYxZjQiLCJlbmRwb2ludCI6Ii92MS9jaGF0L2NvbXBsZXRpb25zIiwibWV0aG9kIjoiUE9TVCIsInNlcnZlZF9hdCI6MTc1MDAwMDAwMCwiZXZlbnRfbG9nIjpbeyJ0eXBlIjoicmVxdWVzdC5yZWNlaXZlZCIsImJvZHlfaGFzaCI6InNoYTI1Njo5NGQ4MDliZjQ3MzgwZDhhMmVhYjBlYjZlMTI2ZDRkZGE5MzY0YjBiNDcyNWNkZjdlYWQ1MmRkNzBiMmFhODdiIn0seyJ0eXBlIjoicmVxdWVzdC5mb3J3YXJkZWQiLCJib2R5X2hhc2giOiJzaGEyNTY6OTRkODA5YmY0NzM4MGQ4YTJlYWIwZWI2ZTEyNmQ0ZGRhOTM2NGIwYjQ3MjVjZGY3ZWFkNTJkZDcwYjJhYTg3YiJ9LHsidHlwZSI6InVwc3RyZWFtLnZlcmlmaWVkIiwicmVzdWx0IjoidmVyaWZpZWQiLCJyZXF1aXJlZCI6dHJ1ZSwibW9kZWxfaWQiOiJkZW1vLW1vZGVsIiwic2Vzc2lvbl9pZCI6InNoYTI1NjphNTk1ZDI2OTcyOGUxNWZlODIzNmFmNDY1ODZmZTg0ZjIyMDY5NmMwZDdkNGU2NDdlZWQzNjkyMmI3YjIwY2I2In0seyJ0eXBlIjoicmVzcG9uc2UucmV0dXJuZWQiLCJib2R5X2hhc2giOiJzaGEyNTY6ZGVkZmZmZTViMTRkMDMxYjhlMmMwMTk5NmQwMjFhMTUyOTNjYjdjNjNiNTZiZTdlNGJlOWU4OWI2ZjBhNWY2MSJ9XX0=\",\"key_id\":\"receipt-1\",\"algo\":\"ed25519\",\"signature\":\"b0b2c830be73d6b6ad9a90b75b9c347a930e6a918e6e4f70ad1c3ce0d3dbfe6789504be5f7d317d24ba9eb84cd8bf634d58e898de89baa7fc939abd12e1b7400\"}";
const REQUEST_AAD_HEX = "6163692e653265652e76332e726571756573740064656d6f2d6d6f64656c0061633031623232303965383633353466623835333233376235646530663466616231336337666362663433336136316330313933363936313766656366313062";
const REQUEST_SHARED_SECRET = "8a7eddf1d2e69d6c895a5e969092dd6be0caa725d435c0244fe33fc2259aa847";
const REQUEST_HKDF_KEY = "85f1bf4e0e1bfea5080911e3d785d1ddbd40c91b7c909a45eef2cd7873bda591";
const REQUEST_SEALED_HEX = "50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d000102030405060708090a0b3216caf5913e5ac1cc3abacc3d9e522872087b64b4588d1846266624899249a17d9ca9e5e730dfe417be0983116a9a855bac522183fbddab9080ed2e086c7aa2ee366c9a2891839ef26bc4f4fa783616408c";
const REQUEST_ENVELOPE = "{\"model\":\"demo-model\",\"sealed_b64\":\"UKYUCbHd0DJemxa3AOcZ6XcsBwALG9d4bpB8ZT0gSV0AAQIDBAUGBwgJCgsyFsr1kT5awcw6usw9nlIocgh7ZLRYjRhGJmYkiZJJoX2cqeXnMN/kF74JgxFqmoVbrFIhg/vdq5CA7S4IbHqi7jZsmiiRg57ya8T0+ng2FkCM\"}";
const RESPONSE_AAD_HEX = "6163692e653265652e76332e726573706f6e73650064656d6f2d6d6f64656c";
const RESPONSE_SHARED_SECRET = "e9bc12821c65b1a542dfe9644cc6111688dde35d821673c0c389fa2bafb70c67";
const RESPONSE_HKDF_KEY = "c8ee880bc3ad7a73151fe26b1f241750d34ac933a34802093b77c3a611b31c59";
const RESPONSE_SEALED_HEX = "f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d68174557101112131415161718191a1b497499f8bc6bb890ecfd49d9e4e886161be04d5014796252171e8e2e67a06dd1c6e181a3c1e105c6762431c9971ed32c58c6";
const RESPONSE_ENVELOPE = "{\"sealed_b64\":\"9bLW5g+Ud+MQwpgtqqbJE2wQihd3xZR+RI+jfWgXRVcQERITFBUWFxgZGhtJdJn4vGu4kOz9Sdnk6IYWG+BNUBR5YlIXHo4uZ6Bt0cbhgaPB4QXGdiQxyZce0yxYxg==\"}";
const SSE_PLAINTEXT = "{\"id\":\"chatcmpl-123\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}";
const SSE_SHARED_SECRET = "5499ce29cc7e5a9fd0f935467bef1515fc07e7463cdcd7833137e1f4ee79d238";
const SSE_HKDF_KEY = "0feb1d49d177d82b8e729df415ec45be84b4833ed75e2b0520786ba3c97eed12";
const SSE_SEALED_HEX = "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d202122232425262728292a2b97d48f43a339f977ac5b2808607cee21003fcabdeed440c426cc57465afc764e9da772ad74762ae683d37b6207561d7cb9ae62e12ecf12423ef76d2e375a9637578819a9b7e4f3a4967aed7ca5dfcade9dea53844d30f2288440d3ba21c2f1dade14576ede947ec0a48dffeaf7ead8380513129a79bf4b";
const SSE_STREAM = "data: {\"sealed_b64\":\"E75P6uryBMf9M1j8nAByGIHRdCeBKCJ+xnTzf3/pe20gISIjJCUmJygpKiuX1I9Dozn5d6xbKAhgfO4hAD/Kve7UQMQmzFdGWvx2Tp2ncq10dirmg9N7YgdWHXy5rmLhLs8SQj73bS43WpY3V4gZqbfk86SWeu18pd/K3p3qU4RNMPIohEDTuiHC8dreFFdu3pR+wKSN/+r36tg4BRMSmnm/Sw==\"}\n\ndata: [DONE]\n\n";
const RECEIPT_PUBLIC_KEY = "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394";
const SERVICE_E2EE_PUBLIC_KEY = "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22";
const CLIENT_E2EE_PUBLIC_KEY = "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b";
const REQUEST_EPHEMERAL_PUBLIC_KEY = "50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d";
const RESPONSE_EPHEMERAL_PUBLIC_KEY = "f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d68174557";
const SSE_EPHEMERAL_PUBLIC_KEY = "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d";

// --- Deterministic §7.1 seal reproduction -----------------------------------------

async function x25519Shared(privateKey: CryptoKey, publicKeyHex: string): Promise<Uint8Array> {
  const publicKey = await subtle.importKey(
    'raw',
    fromHex(publicKeyHex) as BufferSource,
    { name: 'X25519' },
    false,
    [],
  );
  return new Uint8Array(await subtle.deriveBits({ name: 'X25519', public: publicKey }, privateKey, 256));
}

async function hkdfKeyBytes(shared: Uint8Array, context: E2eeContext): Promise<Uint8Array> {
  const ikm = await subtle.importKey('raw', shared as BufferSource, 'HKDF', false, ['deriveBits']);
  return new Uint8Array(
    await subtle.deriveBits(
      {
        name: 'HKDF',
        hash: 'SHA-256',
        salt: new Uint8Array(0) as BufferSource,
        info: enc.encode(context) as BufferSource,
      },
      ikm,
      256,
    ),
  );
}

interface SealPins {
  ephemeralPublicKey: string;
  gcmNonce: string;
  sharedSecret: string;
  hkdfKey: string;
  sealedHex: string;
}

/** Rebuild a §7.1 seal from its pinned ephemeral seed and nonce, checking every intermediate. */
async function reproduceSeal(
  ephemeralSeedHex: string,
  recipientPublicKeyHex: string,
  context: E2eeContext,
  model: string,
  plaintext: string,
  pins: SealPins,
  clientPublicKey?: string,
): Promise<void> {
  const ephemeral = await fx.x25519FromSeed(ephemeralSeedHex);
  assert.equal(ephemeral.publicKeyHex, pins.ephemeralPublicKey);
  assert.equal(pins.sealedHex.slice(0, 64), pins.ephemeralPublicKey);
  assert.equal(pins.sealedHex.slice(64, 88), pins.gcmNonce);

  const shared = await x25519Shared(ephemeral.privateKey, recipientPublicKeyHex);
  assert.equal(toHex(shared), pins.sharedSecret);
  const keyBytes = await hkdfKeyBytes(shared, context);
  assert.equal(toHex(keyBytes), pins.hkdfKey);

  const key = await subtle.importKey('raw', keyBytes as BufferSource, { name: 'AES-GCM' }, false, [
    'encrypt',
  ]);
  const ciphertext = new Uint8Array(
    await subtle.encrypt(
      {
        name: 'AES-GCM',
        iv: fromHex(pins.gcmNonce) as BufferSource,
        additionalData: e2eeAad(context, model, clientPublicKey) as BufferSource,
      },
      key,
      enc.encode(plaintext) as BufferSource,
    ),
  );
  assert.equal(pins.ephemeralPublicKey + pins.gcmNonce + toHex(ciphertext), pins.sealedHex);
}

// --- The pins ----------------------------------------------------------------------

test('vectors: the fixed seeds derive the published public keys', async () => {
  assert.equal((await fx.ed25519FromSeed('02'.repeat(32))).publicKeyHex, RECEIPT_PUBLIC_KEY);
  assert.equal((await fx.x25519FromSeed('03'.repeat(32))).publicKeyHex, SERVICE_E2EE_PUBLIC_KEY);
  assert.equal((await fx.x25519FromSeed('04'.repeat(32))).publicKeyHex, CLIENT_E2EE_PUBLIC_KEY);
  assert.equal((await fx.x25519FromSeed('05'.repeat(32))).publicKeyHex, REQUEST_EPHEMERAL_PUBLIC_KEY);
  assert.equal((await fx.x25519FromSeed('06'.repeat(32))).publicKeyHex, RESPONSE_EPHEMERAL_PUBLIC_KEY);
  assert.equal((await fx.x25519FromSeed('07'.repeat(32))).publicKeyHex, SSE_EPHEMERAL_PUBLIC_KEY);
});

test('vectors §1: keyset bytes → workload_keyset_b64 → digest', async () => {
  const keysetBytes = enc.encode(KEYSET_JSON);
  assert.equal(toBase64(keysetBytes), KEYSET_B64);
  assert.equal(dec.decode(fromBase64(KEYSET_B64)), KEYSET_JSON);
  assert.equal(await computeKeysetDigest(keysetBytes), KEYSET_DIGEST);
  // The self-consistency fixtures build this same keyset — one vector family.
  assert.equal(dec.decode(fx.KEYSET_BYTES), KEYSET_JSON);
});

test('vectors §2: statement bytes and report_data for both nonce forms; report-data slot', async () => {
  assert.equal(dec.decode(attestationStatement(KEYSET_DIGEST, 'test-nonce')), STATEMENT_WITH_NONCE);
  assert.equal(await computeReportData(KEYSET_DIGEST, 'test-nonce'), REPORT_DATA_WITH_NONCE);
  assert.equal(dec.decode(attestationStatement(KEYSET_DIGEST, null)), STATEMENT_NULL_NONCE);
  assert.equal(await computeReportData(KEYSET_DIGEST, null), REPORT_DATA_NULL_NONCE);
  // The 64-byte report-data slot: digest in bytes 0–31, zero in 32–63.
  assert.equal(REPORT_DATA_WITH_NONCE + '00'.repeat(32), REPORT_DATA_SLOT);
});

test('vectors §1–§2: a report assembled from the vectors passes verifyReportBinding', async () => {
  const report = {
    api_version: 'aci/1',
    workload_keyset_digest: KEYSET_DIGEST,
    attestation: {
      tee_type: 'tdx',
      workload_keyset_b64: KEYSET_B64,
      report_data: REPORT_DATA_WITH_NONCE,
    },
  };
  const result = await verifyReportBinding(report, 'test-nonce', { now: 1750000000 });
  assert.equal(result.ok, true);
  assert.equal(result.workloadKeysetDigest, KEYSET_DIGEST);
  assert.equal(result.keyset?.not_after, 1800000000);
});

test('vectors §3: session document bytes → session_id; evidence digest', async () => {
  assert.equal(await computeSessionId(enc.encode(SESSION_JSON)), SESSION_ID);
  const session = JSON.parse(SESSION_JSON) as SessionRecord;
  assert.equal(session.evidence.digest, EVIDENCE_DIGEST);
  assert.equal(session.evidence.data, EVIDENCE_DATA);
  assert.equal(await checkSessionEvidence(session.evidence), true);
  assert.equal(await hashBody('example-evidence'), EVIDENCE_DIGEST);
});

test('vectors §4: body hashes, payload digest, Ed25519 signature, envelope verification', async () => {
  assert.equal(await hashBody(REQUEST_BODY), REQUEST_BODY_HASH);
  assert.equal(await hashBody(RESPONSE_BODY), RESPONSE_BODY_HASH);

  const payloadBytes = enc.encode(PAYLOAD_JSON);
  assert.equal(await sha256Hex(payloadBytes), PAYLOAD_SHA256);

  const envelope = JSON.parse(ENVELOPE_JSON) as ReceiptEnvelope;
  assert.equal(dec.decode(fromBase64(envelope.payload_b64)), PAYLOAD_JSON);
  assert.equal(envelope.signature, SIGNATURE);

  // The pinned signature verifies over exactly the payload bytes, nothing else.
  assert.equal(
    await verifyEd25519(fromHex(RECEIPT_PUBLIC_KEY), fromHex(SIGNATURE), payloadBytes),
    true,
  );
  const tampered = enc.encode(PAYLOAD_JSON.replace('rcpt-0001', 'rcpt-0002'));
  assert.equal(await verifyEd25519(fromHex(RECEIPT_PUBLIC_KEY), fromHex(SIGNATURE), tampered), false);

  const keyset = JSON.parse(KEYSET_JSON) as WorkloadKeyset;
  const result = await verifyReceipt(envelope, keyset, KEYSET_DIGEST);
  assert.equal(result.ok, true);
  assert.ok(result.payload);
  assert.equal(await checkRequestBodyHash(result.payload, REQUEST_BODY), true);
  assert.equal(await checkResponseBodyHash(result.payload, RESPONSE_BODY), true);
  assert.equal(findEvent(result.payload, 'upstream.verified')?.session_id, SESSION_ID);
});

test('vectors §5: AAD bytes for both contexts', () => {
  // The request AAD binds the client key (§7.2); the response AAD does not.
  assert.equal(toHex(e2eeAad(REQUEST_CONTEXT, 'demo-model', CLIENT_E2EE_PUBLIC_KEY)), REQUEST_AAD_HEX);
  assert.equal(toHex(e2eeAad(RESPONSE_CONTEXT, 'demo-model')), RESPONSE_AAD_HEX);
});

test('vectors §5.1: the request seal reproduces byte-for-byte; the service key opens it', async () => {
  await reproduceSeal('05'.repeat(32), SERVICE_E2EE_PUBLIC_KEY, REQUEST_CONTEXT, 'demo-model', REQUEST_BODY, {
    ephemeralPublicKey: REQUEST_EPHEMERAL_PUBLIC_KEY,
    gcmNonce: '000102030405060708090a0b',
    sharedSecret: REQUEST_SHARED_SECRET,
    hkdfKey: REQUEST_HKDF_KEY,
    sealedHex: REQUEST_SEALED_HEX,
  }, CLIENT_E2EE_PUBLIC_KEY);
  const sealed = fromHex(REQUEST_SEALED_HEX);
  assert.equal(JSON.stringify({ model: 'demo-model', sealed_b64: toBase64(sealed) }), REQUEST_ENVELOPE);
  const service = await fx.x25519FromSeed('03'.repeat(32));
  assert.equal(
    dec.decode(await openUnit(service.privateKey, REQUEST_CONTEXT, 'demo-model', sealed, CLIENT_E2EE_PUBLIC_KEY)),
    REQUEST_BODY,
  );
});

test('vectors §5.2: the buffered-response seal reproduces; the client key opens it', async () => {
  await reproduceSeal('06'.repeat(32), CLIENT_E2EE_PUBLIC_KEY, RESPONSE_CONTEXT, 'demo-model', RESPONSE_BODY, {
    ephemeralPublicKey: RESPONSE_EPHEMERAL_PUBLIC_KEY,
    gcmNonce: '101112131415161718191a1b',
    sharedSecret: RESPONSE_SHARED_SECRET,
    hkdfKey: RESPONSE_HKDF_KEY,
    sealedHex: RESPONSE_SEALED_HEX,
  });
  const sealed = fromHex(RESPONSE_SEALED_HEX);
  assert.equal(JSON.stringify({ sealed_b64: toBase64(sealed) }), RESPONSE_ENVELOPE);
  const client = await fx.x25519FromSeed('04'.repeat(32));
  assert.equal(
    dec.decode(await openUnit(client.privateKey, RESPONSE_CONTEXT, 'demo-model', sealed)),
    RESPONSE_BODY,
  );
});

test('vectors §5.3: the SSE-event seal reproduces; plaintext framing; the AAD binds the model', async () => {
  await reproduceSeal('07'.repeat(32), CLIENT_E2EE_PUBLIC_KEY, RESPONSE_CONTEXT, 'demo-model', SSE_PLAINTEXT, {
    ephemeralPublicKey: SSE_EPHEMERAL_PUBLIC_KEY,
    gcmNonce: '202122232425262728292a2b',
    sharedSecret: SSE_SHARED_SECRET,
    hkdfKey: SSE_HKDF_KEY,
    sealedHex: SSE_SEALED_HEX,
  });
  const sealed = fromHex(SSE_SEALED_HEX);
  // The exact wire bytes: sealed data payload, plaintext framing and [DONE] (§7.3).
  assert.equal(
    'data: ' + JSON.stringify({ sealed_b64: toBase64(sealed) }) + '\n\ndata: [DONE]\n\n',
    SSE_STREAM,
  );
  const client = await fx.x25519FromSeed('04'.repeat(32));
  assert.equal(
    dec.decode(await openUnit(client.privateKey, RESPONSE_CONTEXT, 'demo-model', sealed)),
    SSE_PLAINTEXT,
  );
  // A different envelope model changes the AAD: the pinned seal must not open.
  await assert.rejects(() => openUnit(client.privateKey, RESPONSE_CONTEXT, 'other-model', sealed));
});
