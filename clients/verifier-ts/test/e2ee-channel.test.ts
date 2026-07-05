import assert from 'node:assert/strict';
import { test } from 'node:test';

import { openE2eeChannel, requestAad, responseAad, toHex, fromHex } from '../src/index.js';
import type { AttestationReport, ReportVerification } from '../src/index.js';

const ALGO = 'x25519-aes-256-gcm-hkdf-sha256';
const subtle = globalThis.crypto.subtle;
const HKDF_INFO = new TextEncoder().encode('aci.e2ee.v2.x25519');
const bs = (u: Uint8Array): BufferSource => u as BufferSource;

// A minimal "server" holding the service key — an independent decrypt/encrypt so
// the test proves the channel interoperates with a separate implementation.
async function aes(shared: Uint8Array, usage: KeyUsage): Promise<CryptoKey> {
  const hk = await subtle.importKey('raw', bs(shared), 'HKDF', false, ['deriveKey']);
  return subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: bs(new Uint8Array(0)), info: bs(HKDF_INFO) },
    hk, { name: 'AES-GCM', length: 256 }, false, [usage],
  );
}
async function serverDecrypt(priv: CryptoKey, blobHex: string, aad: Uint8Array): Promise<string> {
  const b = fromHex(blobHex);
  const eph = await subtle.importKey('raw', bs(b.slice(0, 32)), { name: 'X25519' }, false, []);
  const shared = new Uint8Array(await subtle.deriveBits({ name: 'X25519', public: eph }, priv, 256));
  const pt = await subtle.decrypt({ name: 'AES-GCM', iv: bs(b.slice(32, 44)), additionalData: bs(aad) }, await aes(shared, 'decrypt'), bs(b.slice(44)));
  return new TextDecoder().decode(new Uint8Array(pt));
}
async function serverEncrypt(clientPubHex: string, text: string, aad: Uint8Array): Promise<string> {
  const eph = (await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits'])) as CryptoKeyPair;
  const ephPub = new Uint8Array(await subtle.exportKey('raw', eph.publicKey));
  const client = await subtle.importKey('raw', bs(fromHex(clientPubHex)), { name: 'X25519' }, false, []);
  const shared = new Uint8Array(await subtle.deriveBits({ name: 'X25519', public: client }, eph.privateKey, 256));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(await subtle.encrypt({ name: 'AES-GCM', iv: bs(iv), additionalData: bs(aad) }, await aes(shared, 'encrypt'), new TextEncoder().encode(text)));
  const blob = new Uint8Array([...ephPub, ...iv, ...ct]);
  return toHex(blob);
}

async function fixture() {
  const service = (await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits'])) as CryptoKeyPair;
  const servicePubHex = toHex(new Uint8Array(await subtle.exportKey('raw', service.publicKey)));
  const digest = 'sha256:' + '00'.repeat(32);
  const report = {
    workload_keyset_digest: digest,
    attestation: { workload_keyset: { e2ee_public_keys: [{ key_id: 'e2ee-1', algo: ALGO, public_key: servicePubHex }] } },
  } as unknown as AttestationReport;
  const verified = { ok: true, workloadKeysetDigest: digest } as ReportVerification;
  return { service, report, verified };
}

test('seal encrypts the request; the service decrypts it under the request AAD', async () => {
  const { service, report, verified } = await fixture();
  const chan = await openE2eeChannel(report, verified);
  const { body, headers } = await chan.seal({ model: 'gpt-x', messages: [{ role: 'user', content: 'hello' }] });

  const nonce = headers['X-E2EE-Nonce']!;
  const ts = Number(headers['X-E2EE-Timestamp']!);
  assert.equal(headers['X-E2EE-Version'], '2');
  assert.ok(/^[0-9a-f]{64}$/.test(nonce));
  const sealed = (body.messages as any[])[0].content as string;
  assert.notEqual(sealed, 'hello');

  const aad = requestAad({ algo: ALGO, model: 'gpt-x', field: 'messages.0.content', nonce, ts });
  assert.equal(await serverDecrypt(service.privateKey, sealed, aad), 'hello');
});

test('open decrypts a response encrypted to the client key under the response AAD', async () => {
  const { report, verified } = await fixture();
  const chan = await openE2eeChannel(report, verified);
  const { headers } = await chan.seal({ model: 'gpt-x', messages: [{ role: 'user', content: 'hi' }] });

  const nonce = headers['X-E2EE-Nonce']!;
  const ts = Number(headers['X-E2EE-Timestamp']!);
  const respAad = responseAad({ algo: ALGO, model: 'gpt-x', id: 'chatcmpl-1', field: 'choices.0.message.content', nonce, ts });
  const encrypted = await serverEncrypt(headers['X-Client-Pub-Key']!, 'the answer', respAad);
  const opened = await chan.open({ id: 'chatcmpl-1', choices: [{ message: { role: 'assistant', content: encrypted } }] });
  assert.equal((opened.choices as any[])[0].message.content, 'the answer');
});

test('openE2eeChannel refuses an unverified report', async () => {
  const { report } = await fixture();
  await assert.rejects(() => openE2eeChannel(report, { ok: false, workloadKeysetDigest: report.workload_keyset_digest } as ReportVerification));
});
