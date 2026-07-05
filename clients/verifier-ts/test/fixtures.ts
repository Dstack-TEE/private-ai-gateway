/**
 * Every value from spec/test-vectors.md §1–§7, in one place so the suite is easy
 * to refresh when the vectors change. Nothing here is imported by `src/` — these
 * are test inputs and the expected byte-for-byte outputs.
 *
 * Field naming tracks the current spec (§8.4 receipt `upstream.verified` and
 * §9.2 session record). If the spec renames those fields, update the objects and
 * expected hashes below together.
 */

import { fromHex, toHex, type PublicKey, type WorkloadKeyset, type Receipt, type SessionRecord } from '../src/index.js';

// --- Fixed keys (test-vectors.md "Fixed keys") ---------------------------------

/** 32 × 0x01 seed; its Ed25519 public key names the workload identity. */
export const IDENTITY_SEED = '01'.repeat(32);
export const IDENTITY_PUBLIC_KEY = '8a88e3dd7409f195fd52db2d3cba5d72ca6709bf1d94121bf3748801b40f6f5c';

/** 32 × 0x02 seed; receipt signing key `receipt-1`. */
export const RECEIPT_SEED = '02'.repeat(32);
export const RECEIPT_PUBLIC_KEY = '8139770ea87d175f56a35466c34c7ecccb8d8a91b4ee37a25df60f5b8fc9b394';

export const IDENTITY_PUBLIC_KEY_OBJ: PublicKey = { algo: 'ed25519', public_key: IDENTITY_PUBLIC_KEY };

// --- §1 workload_id ------------------------------------------------------------

export const WORKLOAD_ID = 'sha256:57c2c8fa98bcf11441f1eff9ef087db67a5560a026082e96903e15365677b8c0';

// --- §2 workload keyset & digest -----------------------------------------------

/**
 * The §2 keyset. The e2ee `public_key` "abab…ab (32 placeholder bytes: 32 × ab)"
 * from the doc expands to the 32-byte hex string below; the tls `spki_sha256` and
 * the identity/receipt keys are shown in full in the doc.
 */
export const KEYSET: WorkloadKeyset = {
  workload_identity: {
    public_key: { algo: 'ed25519', public_key: IDENTITY_PUBLIC_KEY },
    subject: null,
  },
  keyset_epoch: { version: 1, not_after: 1800000000 },
  receipt_signing_keys: [{ key_id: 'receipt-1', algo: 'ed25519', public_key: RECEIPT_PUBLIC_KEY }],
  e2ee_public_keys: [
    { key_id: 'e2ee-1', algo: 'x25519-aes-256-gcm-hkdf-sha256', public_key: 'ab'.repeat(32) },
  ],
  tls_public_keys: [{ spki_sha256: 'c0'.repeat(32), domain: 'api.example.com' }],
};

export const KEYSET_DIGEST =
  'sha256:f2fba7e1b1451e0c0231df624f293407692ef939d3e0e55bca723131bea3f1ff';

// --- §3 attestation statement / report_data ------------------------------------

export const ATTESTATION_NONCE = 'test-nonce';
export const REPORT_DATA_WITH_NONCE =
  '0b8cc28d7e989a88b1e969af20aa2b224afdc2c99f24c97c31a4af330c964ecf';
export const REPORT_DATA_NONCE_OMITTED =
  'e1818eadad3c28375c625e2fa2d2ffd983d2760c84ce17f8527ddcac884c21b9';

// --- §4 endorsement & revocation -----------------------------------------------

export const ENDORSEMENT_SIGNATURE =
  '64e0a4f5d7af28dfdacc102d14c13470b4ddbd90708e190fc0e787f07b36f20eda0ef1f42ea96b8a7f290eb64a918574dc914ce06b6ea023d2153275f06fd201';
export const REVOCATION_SIGNATURE =
  '5f30e02aa53bb628c7f6410636e9f5e33402d2b0b416a6ed278ea3e6e40b48a9af6ba6e5e55abb89a7ad4627eca444a73cad9d25e22bf239c9c6b362d48ed50f';

// --- §5 attested session -------------------------------------------------------

/** Evidence bytes are ASCII `example-evidence`; its SHA-256 is `evidence.digest`. */
export const EVIDENCE_BYTES = 'example-evidence';
export const EVIDENCE_DIGEST =
  'sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d';

/** Channel binding and claims shared by the session record and the receipt's upstream.verified event. */
const CHANNEL_BINDING = [
  {
    origin: 'https://upstream.example.com',
    spki_sha256: 'd1'.repeat(32),
    type: 'tls_spki_sha256',
  },
];
const CLAIMS = {
  gpu_attested: { status: 'unknown' },
  model_weights_provenance: { status: 'unknown' },
  os_known_good: { status: 'unknown' },
  serving_software_known_good: { status: 'unknown' },
  tcb_up_to_date: { status: 'unknown' },
  tee_attested: {
    reason: 'example quote verified',
    source: 'hardware_proven',
    status: 'asserted',
  },
};

/** Wire session record. `identity` is absent (restored to null in the material). */
export const SESSION_RECORD: SessionRecord = {
  upstream_name: 'demo-upstream',
  endpoint: 'https://upstream.example.com',
  verifier_id: 'example/1',
  channel_binding: CHANNEL_BINDING,
  claims: CLAIMS,
  evidence: { digest: EVIDENCE_DIGEST },
};

export const SESSION_ID =
  'as_2e9011abafe00fc2902aaa5dedf8373f14e2c4f1a456b854ddb475413547188e';

// --- §6 receipt ----------------------------------------------------------------

export const REQUEST_BODY = '{"messages":[{"content":"hi","role":"user"}],"model":"demo-model"}';
export const REQUEST_BODY_HASH =
  'sha256:94d809bf47380d8a2eab0eb6e126d4dda9364b0b4725cdf7ead52dd70b2aa87b';

export const RESPONSE_BODY = '{"choices":[],"id":"chatcmpl-123"}';
export const RESPONSE_BODY_HASH =
  'sha256:dedfffe5b14d031b8e2c01996d021a15293cb7c63b56be7e4be9e89b6f0a5f61';

export const RECEIPT_CANONICAL_SHA256 =
  '1cd5a27c330ac3a5a82ac30176ba67349b42a64b00868d75ffd23600bf7e7b7c';
export const RECEIPT_SIGNATURE =
  '0861f62296f57a120620e79fc0e91008a187e3793b65be4a0ed355d4617f305e00a19b32463e1feb464f7220bcd37debd892facddc068eebf5ef02202c31910f';

/** The full §6 receipt, signed by `receipt-1`. */
export const RECEIPT: Receipt = {
  api_version: 'aci/1',
  chat_id: 'chatcmpl-123',
  endpoint: '/v1/chat/completions',
  event_log: [
    { body_hash: REQUEST_BODY_HASH, seq: 0, type: 'request.received' },
    { body_hash: REQUEST_BODY_HASH, seq: 1, type: 'request.forwarded' },
    {
      channel_bindings: CHANNEL_BINDING,
      claims: CLAIMS,
      model_id: 'demo-model',
      provider_claims: null,
      provider_type: null,
      reason: null,
      required: true,
      result: 'verified',
      seq: 2,
      session_id: SESSION_ID,
      type: 'upstream.verified',
      upstream_name: 'demo-upstream',
      url_origin: 'https://upstream.example.com',
      verifier_id: 'example/1',
    },
    {
      cleartext_hash: RESPONSE_BODY_HASH,
      seq: 3,
      type: 'response.returned',
      wire_hash: RESPONSE_BODY_HASH,
    },
  ],
  method: 'POST',
  model: 'demo-model',
  receipt_id: 'rcpt-0001',
  served_at: 1750000000,
  signature: { algo: 'ed25519', key_id: 'receipt-1', value: RECEIPT_SIGNATURE },
  workload_id: WORKLOAD_ID,
  workload_keyset_digest: KEYSET_DIGEST,
};

// --- §7 E2EE AAD ---------------------------------------------------------------

export const E2EE_ALGO = 'x25519-aes-256-gcm-hkdf-sha256';
export const E2EE_NONCE = '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f';
export const E2EE_TIMESTAMP = 1750000000;
export const E2EE_MODEL = 'demo-model';
export const RESPONSE_ID = 'chatcmpl-123';

export const REQUEST_AAD =
  '{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"messages.0.content","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.request.v2","ts":1750000000}';
export const RESPONSE_AAD =
  '{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"choices.0.message.content","id":"chatcmpl-123","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.response.v2","ts":1750000000}';

// --- Test-only Ed25519 key derivation ------------------------------------------

/** PKCS#8 prefix wrapping a raw Ed25519 32-byte seed (OID 1.3.101.112). */
const ED25519_PKCS8_PREFIX = '302e020100300506032b657004220420';

/**
 * Derive an Ed25519 keypair from its 32-byte seed using only Web Crypto — the
 * seed→public-key mapping and signing live only in tests, so `src/` never needs
 * private-key handling. Returns the raw public key (hex) and the importable
 * private key for re-signing the deterministic vectors.
 */
export async function ed25519FromSeed(
  seedHex: string,
): Promise<{ privateKey: CryptoKey; publicKeyHex: string }> {
  const pkcs8 = fromHex(ED25519_PKCS8_PREFIX + seedHex);
  const privateKey = await globalThis.crypto.subtle.importKey(
    'pkcs8',
    pkcs8 as BufferSource,
    { name: 'Ed25519' },
    true,
    ['sign'],
  );
  const jwk = await globalThis.crypto.subtle.exportKey('jwk', privateKey);
  const publicKeyHex = toHex(base64UrlToBytes(jwk.x ?? ''));
  return { privateKey, publicKeyHex };
}

/** Sign a message with an Ed25519 private key, returning the lowercase-hex signature. */
export async function ed25519SignHex(privateKey: CryptoKey, message: Uint8Array): Promise<string> {
  const sig = await globalThis.crypto.subtle.sign({ name: 'Ed25519' }, privateKey, message as BufferSource);
  return toHex(new Uint8Array(sig));
}

function base64UrlToBytes(s: string): Uint8Array {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/');
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}
