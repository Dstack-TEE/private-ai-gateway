/**
 * E2EE channel to a *verified* workload (§7). `openE2eeChannel` refuses unless
 * the report passed {@link verifyReportBinding} — you cannot encrypt to a key
 * that is not in a verified, endorsed keyset. `seal` encrypts the request's
 * content fields to the attested X25519 key and returns the `X-E2EE-*` headers;
 * `open` decrypts a buffered response and `openChunk` decrypts one streamed SSE
 * chunk. All crypto is Web Crypto (X25519, HKDF, AES-GCM) — no dependencies,
 * runs in the browser. secp256k1 is a separate extension (not in Web Crypto).
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
  /** Encrypt a request's content fields; returns the body and `X-E2EE-*` headers. */
  seal(request: Json): Promise<{ body: Json; headers: Record<string, string> }>;
  /** Decrypt a buffered response produced for the most recent `seal`. */
  open(response: Json): Promise<Json>;
  /** Decrypt one streamed SSE chunk (a `chat.completion.chunk` / completion chunk). */
  openChunk(chunk: Json): Promise<Json>;
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
      const encField = (text: string, field: string) =>
        sealField(serviceRaw, enc.encode(text), requestAad({ algo: ALGO, model, field, nonce, ts }));

      const body: Json = { ...request };
      if (Array.isArray(request.messages)) {
        // Whole-content encryption (§7.2) — the universal form for any modality.
        body.messages = await Promise.all(
          (request.messages as Json[]).map(async (m, i) => {
            if (m?.content == null) return m;
            const text = typeof m.content === 'string' ? m.content : JSON.stringify(m.content);
            return { ...m, content: await encField(text, `messages.${i}.content`) };
          }),
        );
      } else if (request.prompt !== undefined) {
        body.prompt = await sealStringOrArray(request.prompt, 'prompt', encField); // completions
      } else if (request.input !== undefined) {
        body.input = await sealStringOrArray(request.input, 'input', encField); // embeddings
      }
      return {
        body,
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
      const decField = responseDecryptor(client.privateKey, sent, textFrom(response.id));
      const body: Json = { ...response };
      if (Array.isArray(response.choices)) {
        body.choices = await Promise.all(
          (response.choices as Json[]).map(async (c, pos) => {
            const i = indexOf(c, pos);
            const out: Json = { ...c };
            if (out.message && typeof out.message === 'object') {
              const m: Json = { ...(out.message as Json) };
              await openStr(m, 'content', `choices.${i}.message.content`, decField);
              await openStr(m, 'reasoning_content', `choices.${i}.message.reasoning_content`, decField);
              if (m.audio && typeof m.audio === 'object') {
                const a: Json = { ...(m.audio as Json) };
                await openStr(a, 'data', `choices.${i}.message.audio.data`, decField);
                m.audio = a;
              }
              out.message = m;
            } else {
              await openStr(out, 'text', `choices.${i}.text`, decField); // completions
            }
            return out;
          }),
        );
      }
      if (Array.isArray(response.data)) {
        // Embeddings: the value is serialized compactly then encrypted (§7.2).
        body.data = await Promise.all(
          (response.data as Json[]).map(async (d, pos) => {
            const i = indexOf(d, pos);
            const out: Json = { ...d };
            if (typeof out.embedding === 'string') {
              out.embedding = JSON.parse(await decField(out.embedding, `data.${i}.embedding`));
            }
            return out;
          }),
        );
      }
      return body;
    },

    async openChunk(chunk) {
      const decField = responseDecryptor(client.privateKey, sent, textFrom(chunk.id));
      const body: Json = { ...chunk };
      if (Array.isArray(chunk.choices)) {
        body.choices = await Promise.all(
          (chunk.choices as Json[]).map(async (c, pos) => {
            const i = indexOf(c, pos);
            const out: Json = { ...c };
            if (out.delta && typeof out.delta === 'object') {
              const d: Json = { ...(out.delta as Json) };
              await openStr(d, 'content', `choices.${i}.delta.content`, decField);
              await openStr(d, 'reasoning_content', `choices.${i}.delta.reasoning_content`, decField);
              out.delta = d;
            } else {
              await openStr(out, 'text', `choices.${i}.text`, decField); // completions stream
            }
            return out;
          }),
        );
      }
      return body;
    },
  };
}

/** A field decryptor bound to the request context (§7.3) and the response `id`. */
function responseDecryptor(
  clientPriv: CryptoKey,
  sent: { model: string; nonce: string; ts: number } | undefined,
  id: string,
): (blobHex: string, field: string) => Promise<string> {
  if (!sent) throw new Error('open: call seal first');
  const { model, nonce, ts } = sent;
  return async (blobHex, field) =>
    dec.decode(await openField(clientPriv, blobHex, responseAad({ algo: ALGO, model, id, field, nonce, ts })));
}

/** `choices`/`data` index is the entry's `index` member, else its array position (§7.2). */
function indexOf(entry: Json, position: number): number {
  return typeof entry.index === 'number' ? entry.index : position;
}

function textFrom(v: unknown): string {
  return typeof v === 'string' ? v : '';
}

/** Decrypt string member `key` of `obj` at `field`, in place; leave non-strings untouched. */
async function openStr(
  obj: Json,
  key: string,
  field: string,
  decField: (blobHex: string, field: string) => Promise<string>,
): Promise<void> {
  if (typeof obj[key] === 'string') obj[key] = await decField(obj[key] as string, field);
}

/** Encrypt a string, or each string element of an array at `name.{i}` (§7.2). */
async function sealStringOrArray(
  value: unknown,
  name: string,
  encField: (text: string, field: string) => Promise<string>,
): Promise<unknown> {
  if (typeof value === 'string') return encField(value, name);
  if (Array.isArray(value)) {
    return Promise.all(value.map((v, i) => (typeof v === 'string' ? encField(v, `${name}.${i}`) : v)));
  }
  return value;
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
