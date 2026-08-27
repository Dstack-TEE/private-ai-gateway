/**
 * A deterministic synthetic ACI service, built with the library's own
 * constructions (self-consistency; spec/test-vectors.md pins land separately).
 * Keys derive from fixed seeds via Web Crypto, so the seed→public-key mapping
 * and signing live only in tests — `src/` never handles private keys.
 */

import {
  toHex,
  toBase64,
  jcsBytes,
  fromHex,
  computeKeysetDigest,
  computeReportData,
  computeSessionId,
  hashBody,
  sha256Hex,
  type AttestationReport,
  type ReceiptEnvelope,
  type ReceiptPayload,
  type SessionRecord,
  type WorkloadKeyset,
} from '../src/index.js';
import { sha384 } from '../src/crypto.js';

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

// --- Keyset (§3.1) ---------------------------------------------------------------

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
  e2ee_public_keys: [{ key_id: 'e2ee-1', algo: 'x25519-aes-256-gcm-hkdf-sha256', public_key: e2eeKey.publicKeyHex }],
  tls_public_keys: [{ spki_sha256: 'c0'.repeat(32), domain: 'api.example.com' }],
};

/** The keyset serialized ONCE — these exact bytes are the artifact (Appendix A, §3.1). */
export const KEYSET_DIGEST = await computeKeysetDigest(KEYSET);

// --- Report (§4.1) -----------------------------------------------------------------

export const NONCE = '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f';
/** Before NOT_AFTER; the fixed clock for deterministic runs. */
export const NOW = 1750001000;

export function makeReport(reportData: string): AttestationReport {
  return {
    api_version: 'aci/1',
    workload_keyset_digest: KEYSET_DIGEST,
    attestation: {
      tee_type: 'tdx',
      workload_keyset: KEYSET,
      report_data: reportData,
      source_provenance: {
        repo_url: 'https://github.com/Dstack-TEE/private-ai-gateway',
        repo_commit: 'f9706ad89220b5d033e38a6a9f1d94121bf37488',
        image_digest: null,
        image_provenance: null,
      },
      evidence: { quote_b64: 'AA==' },
    },
    service_capabilities: { supported_e2ee_versions: ['2'] },
  };
}

export const REPORT = makeReport(await computeReportData(KEYSET_DIGEST, NONCE));

export async function makeMeasuredComposeReport(
  appCompose = 'services:\n  gateway:\n    image: demo\n',
): Promise<{ report: AttestationReport; composeHash: string }> {
  const composeHash = await sha256Hex(enc.encode(appCompose));
  const digests = ['11'.repeat(48), '22'.repeat(48)];
  let rtmr3: Uint8Array = new Uint8Array(48);
  for (const digest of digests) {
    const input = new Uint8Array(96);
    input.set(rtmr3);
    input.set(fromHex(digest), 48);
    rtmr3 = await sha384(input);
  }
  const quote = new Uint8Array(568);
  quote.set(rtmr3, 520);
  const report = structuredClone(REPORT) as AttestationReport;
  report.attestation.evidence = {
    event_log: JSON.stringify([
      { imr: 3, digest: digests[0], event: 'compose-hash', event_payload: composeHash },
      { imr: 3, digest: digests[1], event: 'system-ready', event_payload: '' },
    ]),
    app_compose: appCompose,
    quote: toHex(quote),
  };
  return { report, composeHash };
}

// --- Attested session (§8.2) --------------------------------------------------------

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

/** The session id: the hash of the JCS form of the document (§8). */
export const SESSION_BYTES = enc.encode(JSON.stringify(SESSION));
export const SESSION_ID = await computeSessionId(SESSION);

// --- Receipt (§7) ---------------------------------------------------------------------

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

/** Sign a receipt document (§7.2): signature over JCS(document minus `signature`). */
export async function makeDocument(payload: Record<string, unknown>): Promise<ReceiptEnvelope> {
  const unsigned = { ...payload, key_id: 'receipt-1' };
  return {
    ...unsigned,
    signature: await ed25519SignHex(receiptKey.privateKey, jcsBytes(unsigned)),
  } as ReceiptEnvelope;
}

export const ENVELOPE = await makeDocument(RECEIPT_PAYLOAD as unknown as Record<string, unknown>);
