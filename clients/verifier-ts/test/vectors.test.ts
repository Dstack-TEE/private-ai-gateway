/**
 * Byte-exact pins against spec/test-vectors.md — the cross-implementation
 * check. Every digest, signature, AAD, and sealed unit published there is
 * recomputed here from the vector inputs; the sealing vectors pin the
 * ephemeral keys and GCM nonces a real client draws fresh per unit (§6.1).
 * The constants are copied verbatim from the spec document.
 */

import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  jcsBytes,
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
  type ReceiptEnvelope,
  type SessionRecord,
  type WorkloadKeyset,
} from '../src/index.js';
import * as fx from './fixtures.js';

const subtle = globalThis.crypto.subtle;
const enc = new TextEncoder();
const dec = new TextDecoder();

// --- Constants from spec/test-vectors.md, verbatim -------------------------------

const KEYSET_JCS = "{\"e2ee_public_keys\":[{\"algo\":\"x25519-aes-256-gcm-hkdf-sha256\",\"key_id\":\"e2ee-1\",\"public_key\":\"5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22\"}],\"not_after\":1800000000,\"receipt_signing_keys\":[{\"algo\":\"ed25519\",\"key_id\":\"receipt-1\",\"public_key\":\"8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394\"}],\"subject\":\"dstack-app://example-app\",\"tls_public_keys\":[{\"domain\":\"api.example.com\",\"spki_sha256\":\"c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0\"}]}";
const KEYSET_DIGEST = "sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371";
const STATEMENT_WITH_NONCE = "{\"keyset_digest\":\"sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371\",\"nonce\":\"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f\",\"purpose\":\"aci.report_data.v1\"}";
const REPORT_DATA_WITH_NONCE = "df2174d28130852b413646a3786927b93e94c11d770268b65def8bdba45cb49e";
const STATEMENT_NULL_NONCE = "{\"keyset_digest\":\"sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371\",\"nonce\":null,\"purpose\":\"aci.report_data.v1\"}";
const REPORT_DATA_NULL_NONCE = "0633919ca3f00e97bafaa3304278eb22420cc3ff0d19f87dfca2d3f7508150bc";
const REPORT_DATA_SLOT = "df2174d28130852b413646a3786927b93e94c11d770268b65def8bdba45cb49e0000000000000000000000000000000000000000000000000000000000000000";
const EVIDENCE_DIGEST = "sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d";
const EVIDENCE_DATA = "data:text/plain;base64,ZXhhbXBsZS1ldmlkZW5jZQ==";
const SESSION_JSON = "{\"api_version\":\"aci/1\",\"channel_binding\":[{\"origin\":\"https://upstream.example.com\",\"spki_sha256\":\"d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1\",\"type\":\"tls_spki_sha256\"}],\"claims\":{\"extra\":{\"gpu_arch\":\"HOPPER\",\"tcb_status\":\"UpToDate\"},\"gpu_attested\":{\"status\":\"unknown\"},\"model_weights_provenance\":{\"status\":\"unknown\"},\"os_known_good\":{\"status\":\"unknown\"},\"serving_software_known_good\":{\"status\":\"unknown\"},\"tcb_up_to_date\":{\"status\":\"unknown\"},\"tee_attested\":{\"reason\":\"example quote verified\",\"source\":\"hardware_proven\",\"status\":\"asserted\"}},\"endpoint\":\"https://upstream.example.com\",\"established_at\":1750000000,\"evidence\":{\"data\":\"data:text/plain;base64,ZXhhbXBsZS1ldmlkZW5jZQ==\",\"digest\":\"sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d\"},\"expires_at\":1750003600,\"upstream_name\":\"demo-upstream\",\"verifier_id\":\"example/1\"}";
const SESSION_ID = "95ad1cb4dd25445808c2e9d116caf420b05703730b506395e8fc1ca6faeae28f";
const REQUEST_BODY = "{\"messages\":[{\"content\":\"hi\",\"role\":\"user\"}],\"model\":\"demo-model\"}";
const REQUEST_BODY_HASH = "sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b";
const RESPONSE_BODY = "{\"choices\":[],\"id\":\"chatcmpl-123\"}";
const RESPONSE_BODY_HASH = "sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61";
const SIGNING_INPUT_JSON = "{\"api_version\":\"aci/1\",\"chat_id\":\"chatcmpl-123\",\"endpoint\":\"/v1/chat/completions\",\"event_log\":[{\"body_hash\":\"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b\",\"type\":\"request.received\"},{\"body_hash\":\"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b\",\"type\":\"request.forwarded\"},{\"model_id\":\"demo-model\",\"required\":true,\"result\":\"verified\",\"session_id\":\"95ad1cb4dd25445808c2e9d116caf420b05703730b506395e8fc1ca6faeae28f\",\"type\":\"upstream.verified\"},{\"body_hash\":\"sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61\",\"type\":\"response.returned\"}],\"key_id\":\"receipt-1\",\"method\":\"POST\",\"model\":\"demo-model\",\"receipt_id\":\"rcpt-0001\",\"served_at\":1750000000,\"workload_keyset_digest\":\"sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371\"}";
const SIGNING_INPUT_SHA256 = "1bd328e6880a5a12b3915af95ea32111310e04ab9e21ac3d71ce268e33b965c9";
const SIGNATURE = "d5b005e093bde3b577faf270b7184b09e169cacb0ecb206b103bd2581f997db03da616175454b063323a23ac1dc68f1ce506c2a6eba8aa0561d5e724f0b80c03";
const DOCUMENT_JSON = "{\"api_version\":\"aci/1\",\"chat_id\":\"chatcmpl-123\",\"endpoint\":\"/v1/chat/completions\",\"event_log\":[{\"body_hash\":\"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b\",\"type\":\"request.received\"},{\"body_hash\":\"sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b\",\"type\":\"request.forwarded\"},{\"model_id\":\"demo-model\",\"required\":true,\"result\":\"verified\",\"session_id\":\"95ad1cb4dd25445808c2e9d116caf420b05703730b506395e8fc1ca6faeae28f\",\"type\":\"upstream.verified\"},{\"body_hash\":\"sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61\",\"type\":\"response.returned\"}],\"key_id\":\"receipt-1\",\"method\":\"POST\",\"model\":\"demo-model\",\"receipt_id\":\"rcpt-0001\",\"served_at\":1750000000,\"signature\":\"d5b005e093bde3b577faf270b7184b09e169cacb0ecb206b103bd2581f997db03da616175454b063323a23ac1dc68f1ce506c2a6eba8aa0561d5e724f0b80c03\",\"workload_keyset_digest\":\"sha256:53a5cd44b30dcc51999754c719f2628a041f174ecbf9662a6f8e898a10cd9371\"}";
const REQUEST_AAD_HEX = "6163692e653265652e76322e726571756573740064656d6f2d6d6f64656c0061633031623232303965383633353466623835333233376235646530663466616231336337666362663433336136316330313933363936313766656366313062";
const REQUEST_SHARED_SECRET = "8a7eddf1d2e69d6c895a5e969092dd6be0caa725d435c0244fe33fc2259aa847";
const REQUEST_HKDF_KEY = "8de526a0457c32a4e2b335c6e1b057dc3766850b6c988e34d26f7e6c38bf9918";
const REQUEST_SEALED_HEX = "50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d000102030405060708090a0b75c87de5cf5941399cd0805eb90de5f9f7283c9f27f16778584046996c6da4e21289f23e6755215a06b1eab2c55e3165fbfc29c979e1e5faa715048cf9f57db5f60f3a0e11a40f5e28eecb5d2dd16f878c49";
const REQUEST_ENVELOPE = "{\"model\":\"demo-model\",\"sealed_b64\":\"UKYUCbHd0DJemxa3AOcZ6XcsBwALG9d4bpB8ZT0gSV0AAQIDBAUGBwgJCgt1yH3lz1lBOZzQgF65DeX59yg8nyfxZ3hYQEaZbG2k4hKJ8j5nVSFaBrHqssVeMWX7/CnJeeHl+qcVBIz59X219g86DhGkD14o7stdLdFvh4xJ\"}";
const RESPONSE_AAD_HEX = "6163692e653265652e76322e726573706f6e73650064656d6f2d6d6f64656c";
const RESPONSE_SHARED_SECRET = "e9bc12821c65b1a542dfe9644cc6111688dde35d821673c0c389fa2bafb70c67";
const RESPONSE_HKDF_KEY = "8b88af8dea1ecaa266a5cb57eb785671ff262694b45febc6fbf06f51bb18d21b";
const RESPONSE_SEALED_HEX = "f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d68174557101112131415161718191a1bcad7816cc6e8a4933cae5ae1f63ef6ddd75103d48cf6be8c521a1f8c48882483eb8f84fad7954bfc8a74d66c83f1698e570c";
const RESPONSE_ENVELOPE = "{\"sealed_b64\":\"9bLW5g+Ud+MQwpgtqqbJE2wQihd3xZR+RI+jfWgXRVcQERITFBUWFxgZGhvK14FsxuikkzyuWuH2Pvbd11ED1Iz2voxSGh+MSIgkg+uPhPrXlUv8inTWbIPxaY5XDA==\"}";
const SSE_PLAINTEXT = "{\"id\":\"chatcmpl-123\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}";
const SSE_SHARED_SECRET = "5499ce29cc7e5a9fd0f935467bef1515fc07e7463cdcd7833137e1f4ee79d238";
const SSE_HKDF_KEY = "287f4dff3084c0d738d1287ac3662f15794e2b97129d4cce428ec533cc3533c2";
const SSE_SEALED_HEX = "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d202122232425262728292a2b0e54d4e4b1209c18ab89a6863711c2d03295ff58b3e49e66eda84f79165681f34a09d9264d540b67e86e825ccb0ee1db54ae1c3b5cd37fd0b4c7b61836758ed1e57179f54b9282ffab8e72cb777449e2347f037419a8853c310606b85d1d3e2efc328d9fd448ed552ab3c273d043c9e40fd0fc56813ac5";
const SSE_STREAM = "data: {\"sealed_b64\":\"E75P6uryBMf9M1j8nAByGIHRdCeBKCJ+xnTzf3/pe20gISIjJCUmJygpKisOVNTksSCcGKuJpoY3EcLQMpX/WLPknmbtqE95FlaB80oJ2SZNVAtn6G6CXMsO4dtUrhw7XNN/0LTHthg2dY7R5XF59UuSgv+rjnLLd3RJ4jR/A3QZqIU8MQYGuF0dPi78Mo2f1EjtVSqzwnPQQ8nkD9D8VoE6xQ==\"}\n\ndata: [DONE]\n\n";
const RECEIPT_PUBLIC_KEY = "8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394";
const SERVICE_E2EE_PUBLIC_KEY = "5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22";
const CLIENT_E2EE_PUBLIC_KEY = "ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b";
const REQUEST_EPHEMERAL_PUBLIC_KEY = "50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d";
const RESPONSE_EPHEMERAL_PUBLIC_KEY = "f5b2d6e60f9477e310c2982daaa6c9136c108a1777c5947e448fa37d68174557";
const SSE_EPHEMERAL_PUBLIC_KEY = "13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d";

// --- Deterministic §6.1 seal reproduction -----------------------------------------

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

interface SealPins {
  ephemeralPublicKey: string;
  gcmNonce: string;
  sharedSecret: string;
  hkdfKey: string;
  sealedHex: string;
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

test('vectors §1: keyset object → JCS form → digest', async () => {
  const keyset = JSON.parse(KEYSET_JCS) as Record<string, unknown>;
  assert.equal(dec.decode(jcsBytes(keyset)), KEYSET_JCS);
  assert.equal(await computeKeysetDigest(keyset), KEYSET_DIGEST);
  // The self-consistency fixtures build this same keyset — one vector family.
  assert.equal(await computeKeysetDigest(fx.KEYSET), KEYSET_DIGEST);
});

test('vectors §2: statement bytes and report_data for both nonce forms; report-data slot', async () => {
  assert.equal(dec.decode(attestationStatement(KEYSET_DIGEST, '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f')), STATEMENT_WITH_NONCE);
  assert.equal(await computeReportData(KEYSET_DIGEST, '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f'), REPORT_DATA_WITH_NONCE);
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
      workload_keyset: JSON.parse(KEYSET_JCS) as Record<string, unknown>,
      report_data: REPORT_DATA_WITH_NONCE,
    },
  };
  const result = await verifyReportBinding(report, '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f', { now: 1750000000 });
  assert.equal(result.ok, true);
  assert.equal(result.workloadKeysetDigest, KEYSET_DIGEST);
  assert.equal(result.keyset?.not_after, 1800000000);
});

test('vectors §3: session document bytes → session_id; evidence digest', async () => {
  assert.equal(await computeSessionId(JSON.parse(SESSION_JSON)), SESSION_ID);
  const session = JSON.parse(SESSION_JSON) as SessionRecord;
  assert.equal(session.evidence.digest, EVIDENCE_DIGEST);
  assert.equal(session.evidence.data, EVIDENCE_DATA);
  assert.equal(await checkSessionEvidence(session.evidence), true);
  assert.equal(await hashBody('example-evidence'), EVIDENCE_DIGEST);
});

test('vectors §4: body hashes, signing input, Ed25519 signature, document verification', async () => {
  assert.equal(await hashBody(REQUEST_BODY), REQUEST_BODY_HASH);
  assert.equal(await hashBody(RESPONSE_BODY), RESPONSE_BODY_HASH);

  const document = JSON.parse(DOCUMENT_JSON) as ReceiptEnvelope;
  const { signature, ...unsigned } = document;
  assert.equal(dec.decode(jcsBytes(unsigned)), SIGNING_INPUT_JSON);
  assert.equal(await sha256Hex(jcsBytes(unsigned)), SIGNING_INPUT_SHA256);
  assert.equal(signature, SIGNATURE);

  // The pinned signature verifies over exactly the JCS signing input.
  assert.equal(
    await verifyEd25519(fromHex(RECEIPT_PUBLIC_KEY), fromHex(SIGNATURE), jcsBytes(unsigned)),
    true,
  );
  const tampered = { ...unsigned, receipt_id: 'rcpt-0002' };
  assert.equal(
    await verifyEd25519(fromHex(RECEIPT_PUBLIC_KEY), fromHex(SIGNATURE), jcsBytes(tampered)),
    false,
  );

  const keyset = JSON.parse(KEYSET_JCS) as WorkloadKeyset;
  const result = await verifyReceipt(document, keyset, KEYSET_DIGEST);
  assert.equal(result.ok, true);
  assert.ok(result.payload);
  assert.equal(await checkRequestBodyHash(result.payload, REQUEST_BODY), true);
  assert.equal(await checkResponseBodyHash(result.payload, RESPONSE_BODY), true);
});
