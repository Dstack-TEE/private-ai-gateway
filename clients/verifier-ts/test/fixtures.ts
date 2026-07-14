/**
 * A deterministic synthetic ACI service, built with the library's own
 * constructions (self-consistency; spec/test-vectors.md pins land separately).
 * Keys derive from fixed seeds via Web Crypto, so the seed→public-key mapping
 * and signing live only in tests — `src/` never handles private keys.
 */

import {
  toHex,
  toBase64,
  fromHex,
  computeKeysetDigest,
  computeReportData,
  computeSessionId,
  hashBody,
  E2EE_ALGORITHM,
  type AttestationReport,
  type ReceiptEnvelope,
  type ReceiptPayload,
  type SessionRecord,
  type WorkloadKeyset,
} from '../src/index.js';

const subtle = globalThis.crypto.subtle;
const enc = new TextEncoder();

// --- Test-only key derivation ---------------------------------------------------

/** PKCS#8 prefixes wrapping a raw 32-byte seed (OIDs 1.3.101.112 / 1.3.101.110). */
const ED25519_PKCS8_PREFIX = '302e020100300506032b657004220420';
const X25519_PKCS8_PREFIX = '302e020100300506032b656e04220420';

export interface TestKey {
  privateKey: CryptoKey;
  publicKeyHex: string;
}

async function keyFromSeed(
  prefix: string,
  seedHex: string,
  algorithm: string,
  usages: KeyUsage[],
): Promise<TestKey> {
  const pkcs8 = fromHex(prefix + seedHex);
  const privateKey = await subtle.importKey('pkcs8', pkcs8 as BufferSource, { name: algorithm }, true, usages);
  const jwk = await subtle.exportKey('jwk', privateKey);
  return { privateKey, publicKeyHex: toHex(base64UrlToBytes(jwk.x ?? '')) };
}

export function ed25519FromSeed(seedHex: string): Promise<TestKey> {
  return keyFromSeed(ED25519_PKCS8_PREFIX, seedHex, 'Ed25519', ['sign']);
}

export function x25519FromSeed(seedHex: string): Promise<TestKey> {
  return keyFromSeed(X25519_PKCS8_PREFIX, seedHex, 'X25519', ['deriveBits']);
}

/** Sign a message with an Ed25519 private key, returning the lowercase-hex signature. */
export async function ed25519SignHex(privateKey: CryptoKey, message: Uint8Array): Promise<string> {
  const sig = await subtle.sign({ name: 'Ed25519' }, privateKey, message as BufferSource);
  return toHex(new Uint8Array(sig));
}

function base64UrlToBytes(s: string): Uint8Array {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/');
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

// --- Keyset (§4.1) ---------------------------------------------------------------

export const RECEIPT_SEED = '02'.repeat(32);
export const E2EE_SEED = '03'.repeat(32);

export const receiptKey = await ed25519FromSeed(RECEIPT_SEED);
export const e2eeKey = await x25519FromSeed(E2EE_SEED);

export const NOT_AFTER = 1800000000;

export const KEYSET: WorkloadKeyset = {
  subject: 'dstack-app://example-app',
  not_after: NOT_AFTER,
  receipt_signing_keys: [
    { key_id: 'receipt-1', algo: 'ed25519', public_key: receiptKey.publicKeyHex },
  ],
  e2ee_public_keys: [{ key_id: 'e2ee-1', algo: E2EE_ALGORITHM, public_key: e2eeKey.publicKeyHex }],
  tls_public_keys: [{ spki_sha256: 'c0'.repeat(32), domain: 'api.example.com' }],
};

/** The keyset serialized ONCE — these exact bytes are the artifact (§3, §4.1). */
export const KEYSET_BYTES = enc.encode(JSON.stringify(KEYSET));
export const KEYSET_B64 = toBase64(KEYSET_BYTES);
export const KEYSET_DIGEST = await computeKeysetDigest(KEYSET_BYTES);

// --- Report (§5.1) -----------------------------------------------------------------

export const NONCE = 'test-nonce';
/** Before NOT_AFTER; the fixed clock for deterministic runs. */
export const NOW = 1750001000;

export function makeReport(reportData: string): AttestationReport {
  return {
    api_version: 'aci/1',
    workload_keyset_digest: KEYSET_DIGEST,
    attestation: {
      tee_type: 'tdx',
      workload_keyset_b64: KEYSET_B64,
      report_data: reportData,
      source_provenance: {
        repo_url: 'https://github.com/Dstack-TEE/private-ai-gateway',
        repo_commit: 'f9706ad89220b5d033e38a6a9f1d94121bf3748801b40f6f5c8a88e3',
        image_digest: null,
        image_provenance: null,
      },
      evidence: { quote_b64: 'AA==' },
    },
    service_capabilities: { supported_e2ee_versions: ['3'] },
  };
}

export const REPORT = makeReport(await computeReportData(KEYSET_DIGEST, NONCE));

// --- Attested session (§9.2) --------------------------------------------------------

export const EVIDENCE_BYTES = enc.encode('example-evidence');

export const SESSION: SessionRecord = {
  api_version: 'aci/1',
  upstream_name: 'demo-upstream',
  endpoint: 'https://upstream.example.com',
  verifier_id: 'example/1',
  established_at: 1750000000,
  expires_at: 1750003600,
  identity: { signing_address: '0x1234' },
  channel_binding: [
    {
      type: 'tls_spki_sha256',
      origin: 'https://upstream.example.com',
      spki_sha256: 'd1'.repeat(32),
    },
  ],
  claims: {
    tee_attested: { status: 'asserted', source: 'hardware_proven', reason: 'example quote verified' },
    gpu_attested: { status: 'unknown' },
    extra: { tcb_status: 'UpToDate' },
  },
  evidence: {
    digest: await hashBody(EVIDENCE_BYTES),
    data: 'data:application/octet-stream;base64,' + toBase64(EVIDENCE_BYTES),
  },
};

/** The session document serialized ONCE — its id is the hash of these bytes (§9). */
export const SESSION_BYTES = enc.encode(JSON.stringify(SESSION));
export const SESSION_ID = await computeSessionId(SESSION_BYTES);

// --- Receipt (§8) ---------------------------------------------------------------------

export const REQUEST_BODY = '{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}';
export const RESPONSE_BODY = '{"choices":[],"id":"chatcmpl-123"}';

export const RECEIPT_PAYLOAD: ReceiptPayload = {
  api_version: 'aci/1',
  receipt_id: 'rcpt-0001',
  chat_id: 'chatcmpl-123',
  model: 'demo-model',
  workload_keyset_digest: KEYSET_DIGEST,
  endpoint: '/v1/chat/completions',
  method: 'POST',
  served_at: 1750000000,
  event_log: [
    { type: 'request.received', body_hash: await hashBody(REQUEST_BODY) },
    { type: 'request.forwarded', body_hash: await hashBody(REQUEST_BODY) },
    {
      type: 'upstream.verified',
      result: 'verified',
      required: true,
      model_id: 'demo-model',
      session_id: SESSION_ID,
    },
    { type: 'response.returned', body_hash: await hashBody(RESPONSE_BODY) },
  ],
};

/** Build a signed envelope over exact payload bytes with the fixture receipt key. */
export async function makeEnvelope(payloadBytes: Uint8Array): Promise<ReceiptEnvelope> {
  return {
    payload_b64: toBase64(payloadBytes),
    key_id: 'receipt-1',
    algo: 'ed25519',
    signature: await ed25519SignHex(receiptKey.privateKey, payloadBytes),
  };
}

export const PAYLOAD_BYTES = enc.encode(JSON.stringify(RECEIPT_PAYLOAD));
export const ENVELOPE = await makeEnvelope(PAYLOAD_BYTES);
