/**
 * E2EE v3 (§7): seal whole request bodies to the attested X25519 key, open
 * sealed responses — buffered or streamed. All crypto is Web Crypto (X25519,
 * HKDF-SHA256, AES-256-GCM): no dependencies, runs in the browser.
 * {@link openE2eeChannel} refuses an unverified report, so a client cannot
 * encrypt to a key that was never bound through attestation.
 */

import { toHex, fromHex, toBase64, fromBase64 } from './crypto.js';
import { AciError, AciFormatError } from './errors.js';
import type { KeysetKey, ReportVerification } from './types.js';

/** The one E2EE algorithm ACI v1 defines (Appendix A). */
export const E2EE_ALGORITHM = 'x25519-aes-256-gcm-hkdf-sha256';

/** The §7.1 context strings — HKDF info and the first AAD component. */
export type E2eeContext = 'aci.e2ee.v3.request' | 'aci.e2ee.v3.response';
export const REQUEST_CONTEXT: E2eeContext = 'aci.e2ee.v3.request';
export const RESPONSE_CONTEXT: E2eeContext = 'aci.e2ee.v3.response';

const subtle = globalThis.crypto.subtle;
const enc = new TextEncoder();
const dec = new TextDecoder();
const bs = (u: Uint8Array): BufferSource => u as BufferSource;

/** The §7.1 AAD. The request appends `0x00 || client key hex` so a replay
 * cannot reseal to another recipient; the response omits it (§7.2). */
export function e2eeAad(context: E2eeContext, model: string, clientPublicKey?: string): Uint8Array {
  const ctx = enc.encode(context);
  const mdl = enc.encode(model);
  const client = clientPublicKey === undefined ? undefined : enc.encode(clientPublicKey);
  const out = new Uint8Array(ctx.length + 1 + mdl.length + (client ? 1 + client.length : 0));
  out.set(ctx);
  out[ctx.length] = 0x00;
  out.set(mdl, ctx.length + 1);
  if (client) {
    out[ctx.length + 1 + mdl.length] = 0x00;
    out.set(client, ctx.length + 2 + mdl.length);
  }
  return out;
}

/** HKDF-SHA256(salt absent, ikm = shared secret, info = context) → AES-256-GCM key (§7.1). */
async function aesKey(shared: Uint8Array, context: E2eeContext, usage: KeyUsage): Promise<CryptoKey> {
  const hk = await subtle.importKey('raw', bs(shared), 'HKDF', false, ['deriveKey']);
  return subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: bs(new Uint8Array(0)), info: bs(enc.encode(context)) },
    hk,
    { name: 'AES-GCM', length: 256 },
    false,
    [usage],
  );
}

/**
 * Seal one unit (§7.1) to `recipientPublicKey` (raw 32-byte X25519) with a
 * fresh ephemeral key and GCM nonce. Returns the sealed bytes:
 * `ephemeral_public_key (32) || gcm_nonce (12) || ciphertext || tag (16)`.
 */
export async function sealUnit(
  recipientPublicKey: Uint8Array,
  context: E2eeContext,
  model: string,
  plaintext: Uint8Array,
  clientPublicKey?: string,
): Promise<Uint8Array> {
  if (recipientPublicKey.length !== 32) {
    throw new AciFormatError('recipient public key is not 32 bytes');
  }
  const eph = (await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits'])) as CryptoKeyPair;
  const ephPub = new Uint8Array(await subtle.exportKey('raw', eph.publicKey));
  const recipient = await subtle.importKey('raw', bs(recipientPublicKey), { name: 'X25519' }, false, []);
  const shared = new Uint8Array(
    await subtle.deriveBits({ name: 'X25519', public: recipient }, eph.privateKey, 256),
  );
  const gcmNonce = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(
    await subtle.encrypt(
      { name: 'AES-GCM', iv: bs(gcmNonce), additionalData: bs(e2eeAad(context, model, clientPublicKey)) },
      await aesKey(shared, context, 'encrypt'),
      bs(plaintext),
    ),
  );
  const sealed = new Uint8Array(32 + 12 + ct.length);
  sealed.set(ephPub);
  sealed.set(gcmNonce, 32);
  sealed.set(ct, 44);
  return sealed;
}

/**
 * Open one sealed unit addressed to `recipientPrivateKey` (an X25519
 * `CryptoKey` with `deriveBits` usage). Throws {@link AciFormatError} on a
 * malformed unit; AEAD authentication failure rejects with the Web Crypto
 * `OperationError`.
 */
export async function openUnit(
  recipientPrivateKey: CryptoKey,
  context: E2eeContext,
  model: string,
  sealed: Uint8Array,
  clientPublicKey?: string,
): Promise<Uint8Array> {
  if (sealed.length < 32 + 12 + 16) {
    throw new AciFormatError(`sealed unit too short: ${sealed.length} bytes`);
  }
  const ephPub = await subtle.importKey('raw', bs(sealed.slice(0, 32)), { name: 'X25519' }, false, []);
  const shared = new Uint8Array(
    await subtle.deriveBits({ name: 'X25519', public: ephPub }, recipientPrivateKey, 256),
  );
  const pt = await subtle.decrypt(
    { name: 'AES-GCM', iv: bs(sealed.slice(32, 44)), additionalData: bs(e2eeAad(context, model, clientPublicKey)) },
    await aesKey(shared, context, 'decrypt'),
    bs(sealed.slice(44)),
  );
  return new Uint8Array(pt);
}

/** One sealed request: the envelope to send, and the openers for its responses. */
export interface SealedRequest {
  /** The request body to send: the `{"model":…,"sealed_b64":…}` envelope (§7.2). */
  body: string;
  /** The three E2EE request headers (§6.1). */
  headers: Record<string, string>;
  /** Open a buffered sealed response body; returns the original response bytes (§7.3). */
  open(responseBody: Uint8Array | string): Promise<Uint8Array>;
  /**
   * Open one SSE event's data payload; the plaintext `[DONE]` sentinel passes
   * through unchanged (§7.3). SSE framing is the caller's (plaintext) concern.
   */
  openStreamEvent(data: string): Promise<string>;
}

/** An E2EE channel to one verified workload. */
export interface E2eeChannel {
  /** The attested service key requests are sealed to (`X-Model-Pub-Key`). */
  serviceKey: KeysetKey;
  /** This channel's X25519 public key (`X-Client-Pub-Key`), hex. */
  clientPublicKey: string;
  /** Seal one request's exact body bytes (§7.2). */
  seal(requestBody: Uint8Array | string): Promise<SealedRequest>;
}

/**
 * Open an E2EE channel to the workload a passing {@link verifyReportBinding}
 * result describes, selecting its attested {@link E2EE_ALGORITHM} key.
 * Responses are sealed to a fresh channel key; each request seals the caller's
 * exact body bytes, so the receipt's `request.received` hash stays reproducible.
 */
export async function openE2eeChannel(verification: ReportVerification): Promise<E2eeChannel> {
  if (!verification.ok || !verification.keyset) {
    throw new AciError('openE2eeChannel: report is not verified — run verifyReportBinding and check .ok');
  }
  const serviceKey = verification.keyset.e2ee_public_keys.find((k) => k.algo === E2EE_ALGORITHM);
  if (!serviceKey) {
    throw new AciError(`openE2eeChannel: no attested ${E2EE_ALGORITHM} key in the keyset`);
  }
  const serviceRaw = fromHex(serviceKey.public_key);
  if (serviceRaw.length !== 32) {
    throw new AciFormatError('service E2EE public key is not 32 bytes');
  }

  // Channel client key: responses are sealed to it; the service always uses
  // fresh ephemerals per sealed unit (§7.1).
  const client = (await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits'])) as CryptoKeyPair;
  const clientPublicKey = toHex(new Uint8Array(await subtle.exportKey('raw', client.publicKey)));

  return {
    serviceKey,
    clientPublicKey,
    async seal(requestBody) {
      const bytes = typeof requestBody === 'string' ? enc.encode(requestBody) : requestBody;
      let model: unknown;
      try {
        model = (JSON.parse(dec.decode(bytes)) as { model?: unknown }).model;
      } catch {
        throw new AciFormatError('seal: request body is not valid JSON');
      }
      if (typeof model !== 'string') {
        throw new AciFormatError('seal: request body has no string "model" (§7.2)');
      }
      const sealed = await sealUnit(serviceRaw, REQUEST_CONTEXT, model, bytes, clientPublicKey);
      const openSealed = (envelope: { sealed_b64?: unknown }): Promise<Uint8Array> => {
        if (typeof envelope.sealed_b64 !== 'string') {
          throw new AciFormatError('response envelope has no sealed_b64 (§7.3)');
        }
        return openUnit(client.privateKey, RESPONSE_CONTEXT, model, fromBase64(envelope.sealed_b64));
      };
      return {
        body: JSON.stringify({ model, sealed_b64: toBase64(sealed) }),
        headers: {
          'X-E2EE-Version': '3',
          'X-Client-Pub-Key': clientPublicKey,
          'X-Model-Pub-Key': serviceKey.public_key,
        },
        async open(responseBody) {
          const text = typeof responseBody === 'string' ? responseBody : dec.decode(responseBody);
          return openSealed(JSON.parse(text) as { sealed_b64?: unknown });
        },
        async openStreamEvent(data) {
          if (data.trim() === '[DONE]') return data;
          return dec.decode(await openSealed(JSON.parse(data) as { sealed_b64?: unknown }));
        },
      };
    },
  };
}
