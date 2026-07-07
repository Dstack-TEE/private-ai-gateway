/**
 * E2EE channel to a *verified* workload (§7). `openE2eeChannel` refuses unless
 * the report passed {@link verifyReportBinding} — you cannot encrypt to a key
 * that is not in a verified, endorsed keyset. `seal` encrypts the request's
 * message contents to the attested X25519 key and returns the `X-E2EE-*`
 * headers; `open` decrypts the reply. All crypto is Web Crypto (X25519, HKDF,
 * AES-GCM) — no dependencies, runs in the browser. secp256k1 is a separate
 * extension (not in the Web Crypto API).
 */

import { requestAad, responseAad } from './e2ee.js';
import { toHex, fromHex } from './crypto.js';
import type { AttestationReport, ReportVerification } from './types.js';

const ALGO = 'x25519-aes-256-gcm-hkdf-sha256';
const subtle = globalThis.crypto.subtle;
const enc = new TextEncoder();
const dec = new TextDecoder();
const HKDF_INFO = enc.encode('aci.e2ee.v2.x25519');

type Json = Record<string, unknown>;

/** An encrypted channel bound to one verified workload. */
export interface E2eeChannel {
  /** Encrypt a chat request's message contents; returns the body and `X-E2EE-*` headers. */
  seal(request: Json): Promise<{ body: Json; headers: Record<string, string> }>;
  /** Decrypt the buffered chat response produced for the most recent `seal`. */
  open(response: Json): Promise<Json>;
}

/**
 * Open an E2EE channel to the workload `report` describes, once `verification`
 * (from {@link verifyReportBinding} for that report) has passed.
 */
export async function openE2eeChannel(
  report: AttestationReport,
  verification: ReportVerification,
): Promise<E2eeChannel> {
  if (!verification.ok || verification.workloadKeysetDigest !== report.workload_keyset_digest) {
    throw new Error('openE2eeChannel: report is not verified — call verifyReportBinding and check .ok');
  }
  const keys = (report.attestation.workload_keyset.e2ee_public_keys ?? []) as Array<{
    algo: string;
    public_key: string;
  }>;
  const service = keys.find((k) => k?.algo === ALGO);
  if (!service) throw new Error(`openE2eeChannel: no attested ${ALGO} key in the keyset`);
  const serviceRaw = fromHex(service.public_key);

  // Static client key: responses are encrypted to it, and we decrypt with its private half.
  const client = (await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits'])) as CryptoKeyPair;
  const clientPubHex = toHex(new Uint8Array(await subtle.exportKey('raw', client.publicKey)));

  let sent: { model: string; nonce: string; ts: number } | undefined;

  return {
    async seal(request) {
      const model = request.model;
      if (typeof model !== 'string') throw new Error('seal: request.model must be a string');
      const nonce = toHex(crypto.getRandomValues(new Uint8Array(32)));
      const ts = Math.floor(Date.now() / 1000);
      sent = { model, nonce, ts };
      const messages = (request.messages as Json[] | undefined) ?? [];
      const out = await Promise.all(
        messages.map(async (m, i) => {
          if (m?.content == null) return m;
          const field = `messages.${i}.content`;
          const plaintext = typeof m.content === 'string' ? m.content : JSON.stringify(m.content);
          const aad = requestAad({ algo: ALGO, model, field, nonce, ts });
          return { ...m, content: await sealField(serviceRaw, enc.encode(plaintext), aad) };
        }),
      );
      return {
        body: { ...request, messages: out },
        headers: {
          'X-E2EE-Version': '2',
          'X-Client-Pub-Key': clientPubHex,
          'X-Model-Pub-Key': service.public_key,
          'X-E2EE-Nonce': nonce,
          'X-E2EE-Timestamp': String(ts),
        },
      };
    },

    async open(response) {
      if (!sent) throw new Error('open: call seal first');
      const id = typeof response.id === 'string' ? response.id : '';
      const choices = (response.choices as Json[] | undefined) ?? [];
      const out = await Promise.all(
        choices.map(async (c, i) => {
          const message = c?.message as Json | undefined;
          if (!message) return c;
          const opened: Json = { ...message };
          for (const f of ['content', 'reasoning_content']) {
            if (typeof opened[f] !== 'string') continue;
            const field = `choices.${i}.message.${f}`;
            const aad = responseAad({ algo: ALGO, model: sent!.model, id, field, nonce: sent!.nonce, ts: sent!.ts });
            opened[f] = dec.decode(await openField(client.privateKey, opened[f] as string, aad));
          }
          return { ...c, message: opened };
        }),
      );
      return { ...response, choices: out };
    },
  };
}

// `Uint8Array` → `BufferSource` (Web Crypto typings friction; see crypto.ts).
const bs = (u: Uint8Array): BufferSource => u as BufferSource;

/** Derive the AES-256-GCM key from a raw X25519 shared secret (spec §7.1). */
async function aesKey(shared: Uint8Array, usage: KeyUsage): Promise<CryptoKey> {
  const hk = await subtle.importKey('raw', bs(shared), 'HKDF', false, ['deriveKey']);
  return subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: bs(new Uint8Array(0)), info: bs(HKDF_INFO) },
    hk,
    { name: 'AES-GCM', length: 256 },
    false,
    [usage],
  );
}

/** Encrypt one field to `serviceRaw` with a fresh ephemeral key → wire hex. */
async function sealField(serviceRaw: Uint8Array, plaintext: Uint8Array, aad: Uint8Array): Promise<string> {
  const eph = (await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits'])) as CryptoKeyPair;
  const ephPub = new Uint8Array(await subtle.exportKey('raw', eph.publicKey));
  const service = await subtle.importKey('raw', bs(serviceRaw), { name: 'X25519' }, false, []);
  const shared = new Uint8Array(await subtle.deriveBits({ name: 'X25519', public: service }, eph.privateKey, 256));
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(await subtle.encrypt({ name: 'AES-GCM', iv: bs(iv), additionalData: bs(aad) }, await aesKey(shared, 'encrypt'), bs(plaintext)));
  const blob = new Uint8Array(ephPub.length + iv.length + ct.length);
  blob.set(ephPub);
  blob.set(iv, ephPub.length);
  blob.set(ct, ephPub.length + iv.length);
  return toHex(blob);
}

/** Decrypt one field addressed to the client static key. */
async function openField(clientPriv: CryptoKey, blobHex: string, aad: Uint8Array): Promise<Uint8Array> {
  const blob = fromHex(blobHex);
  const ephPub = await subtle.importKey('raw', bs(blob.slice(0, 32)), { name: 'X25519' }, false, []);
  const shared = new Uint8Array(await subtle.deriveBits({ name: 'X25519', public: ephPub }, clientPriv, 256));
  const pt = await subtle.decrypt({ name: 'AES-GCM', iv: bs(blob.slice(32, 44)), additionalData: bs(aad) }, await aesKey(shared, 'decrypt'), bs(blob.slice(44)));
  return new Uint8Array(pt);
}
