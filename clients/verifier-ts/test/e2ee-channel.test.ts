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

test('open decrypts all buffered chat fields: content, reasoning_content, audio.data', async () => {
  const { report, verified } = await fixture();
  const chan = await openE2eeChannel(report, verified);
  const { headers } = await chan.seal({ model: 'gpt-x', messages: [{ role: 'user', content: 'hi' }] });
  const nonce = headers['X-E2EE-Nonce']!, ts = Number(headers['X-E2EE-Timestamp']!), pub = headers['X-Client-Pub-Key']!;
  const aad = (field: string) => responseAad({ algo: ALGO, model: 'gpt-x', id: 'r1', field, nonce, ts });
  const opened: any = await chan.open({
    id: 'r1',
    choices: [{
      index: 0,
      message: {
        role: 'assistant',
        content: await serverEncrypt(pub, 'answer', aad('choices.0.message.content')),
        reasoning_content: await serverEncrypt(pub, 'because', aad('choices.0.message.reasoning_content')),
        audio: { id: 'a', data: await serverEncrypt(pub, 'AUDIO64', aad('choices.0.message.audio.data')) },
      },
    }],
  });
  assert.equal(opened.choices[0].message.content, 'answer');
  assert.equal(opened.choices[0].message.reasoning_content, 'because');
  assert.equal(opened.choices[0].message.audio.data, 'AUDIO64');
});

test('open decrypts completions text and embeddings embedding', async () => {
  const { report, verified } = await fixture();
  const chan = await openE2eeChannel(report, verified);
  const { headers } = await chan.seal({ model: 'gpt-x', prompt: 'hi' });
  const nonce = headers['X-E2EE-Nonce']!, ts = Number(headers['X-E2EE-Timestamp']!), pub = headers['X-Client-Pub-Key']!;

  const cAad = responseAad({ algo: ALGO, model: 'gpt-x', id: 'c1', field: 'choices.0.text', nonce, ts });
  const oc: any = await chan.open({ id: 'c1', choices: [{ index: 0, text: await serverEncrypt(pub, 'done', cAad) }] });
  assert.equal(oc.choices[0].text, 'done');

  // Embeddings carry no `id`; the value is compact JSON, encrypted (§7.2).
  const eAad = responseAad({ algo: ALGO, model: 'gpt-x', id: '', field: 'data.0.embedding', nonce, ts });
  const oe: any = await chan.open({ data: [{ index: 0, embedding: await serverEncrypt(pub, JSON.stringify([0.5, -0.25]), eAad) }] });
  assert.deepEqual(oe.data[0].embedding, [0.5, -0.25]);
});

test('openChunk decrypts streamed chat deltas (content, reasoning_content)', async () => {
  const { report, verified } = await fixture();
  const chan = await openE2eeChannel(report, verified);
  const { headers } = await chan.seal({ model: 'gpt-x', stream: true, messages: [{ role: 'user', content: 'hi' }] });
  const nonce = headers['X-E2EE-Nonce']!, ts = Number(headers['X-E2EE-Timestamp']!), pub = headers['X-Client-Pub-Key']!;
  const aad = (field: string) => responseAad({ algo: ALGO, model: 'gpt-x', id: 's1', field, nonce, ts });

  const o1: any = await chan.openChunk({ id: 's1', choices: [{ index: 0, delta: { content: await serverEncrypt(pub, 'hel', aad('choices.0.delta.content')) } }] });
  const o2: any = await chan.openChunk({ id: 's1', choices: [{ index: 0, delta: { reasoning_content: await serverEncrypt(pub, 'think', aad('choices.0.delta.reasoning_content')) } }] });
  assert.equal(o1.choices[0].delta.content, 'hel');
  assert.equal(o2.choices[0].delta.reasoning_content, 'think');
});

test('seal encrypts completions prompt and embeddings input (string and array)', async () => {
  const { service, report, verified } = await fixture();
  const chan = await openE2eeChannel(report, verified);

  const s1 = await chan.seal({ model: 'gpt-x', prompt: 'hello' });
  const n1 = s1.headers['X-E2EE-Nonce']!, t1 = Number(s1.headers['X-E2EE-Timestamp']!);
  const pAad = requestAad({ algo: ALGO, model: 'gpt-x', field: 'prompt', nonce: n1, ts: t1 });
  assert.equal(await serverDecrypt(service.privateKey, s1.body.prompt as string, pAad), 'hello');

  const s2 = await chan.seal({ model: 'gpt-x', input: ['a', 'b'] });
  const n2 = s2.headers['X-E2EE-Nonce']!, t2 = Number(s2.headers['X-E2EE-Timestamp']!);
  const arr = s2.body.input as string[];
  assert.equal(await serverDecrypt(service.privateKey, arr[0]!, requestAad({ algo: ALGO, model: 'gpt-x', field: 'input.0', nonce: n2, ts: t2 })), 'a');
  assert.equal(await serverDecrypt(service.privateKey, arr[1]!, requestAad({ algo: ALGO, model: 'gpt-x', field: 'input.1', nonce: n2, ts: t2 })), 'b');
});

test('openE2eeChannel refuses an unverified report', async () => {
  const { report } = await fixture();
  await assert.rejects(() => openE2eeChannel(report, { ok: false, workloadKeysetDigest: report.workload_keyset_digest } as ReportVerification));
});
