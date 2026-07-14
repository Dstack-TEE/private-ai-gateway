/**
 * Cryptographic primitives, all via the Web Crypto API (`globalThis.crypto`) so
 * the same code runs in browsers and in Node 20+ with no dependencies. ACI's
 * only signature algorithm is Ed25519 and its only hash is SHA-256 (spec
 * Appendix A) — both are in Web Crypto, so nothing needs injecting.
 */

import { AciFormatError } from './errors.js';

const subtle = globalThis.crypto.subtle;

/** Lowercase-hex encode bytes. */
export function toHex(bytes: Uint8Array): string {
  let out = '';
  for (const b of bytes) out += b.toString(16).padStart(2, '0');
  return out;
}

/** Decode hex (optionally `0x`-prefixed) to bytes. */
export function fromHex(hex: string): Uint8Array {
  const h = hex.startsWith('0x') || hex.startsWith('0X') ? hex.slice(2) : hex;
  if (h.length % 2 !== 0) {
    throw new AciFormatError(`hex string has odd length: ${hex.length} chars`);
  }
  const out = new Uint8Array(h.length / 2);
  for (let i = 0; i < out.length; i++) {
    const byte = Number.parseInt(h.slice(i * 2, i * 2 + 2), 16);
    if (Number.isNaN(byte)) {
      throw new AciFormatError(`invalid hex at offset ${i * 2}: "${h.slice(i * 2, i * 2 + 2)}"`);
    }
    out[i] = byte;
  }
  return out;
}

/** Encode bytes as standard base64 (RFC 4648 §4, with padding) — the `_b64` field form (§3). */
export function toBase64(bytes: Uint8Array): string {
  let bin = '';
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin);
}

/** Decode standard base64 to the exact underlying bytes. */
export function fromBase64(b64: string): Uint8Array {
  let bin: string;
  try {
    bin = atob(b64);
  } catch {
    throw new AciFormatError('invalid base64');
  }
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** SHA-256 of the given bytes. */
export async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(await subtle.digest('SHA-256', bytes as BufferSource));
}

/** SHA-384 of the given bytes — the dstack RTMR replay hash (§10.1 profile). */
export async function sha384(bytes: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(await subtle.digest('SHA-384', bytes as BufferSource));
}

/** Lowercase-hex SHA-256 of the given bytes. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  return toHex(await sha256(bytes));
}

/**
 * `sha256:<lowercase-hex>` digest string of the given bytes — the ACI digest
 * form (§3) used for keyset digests, body hashes, and session ids.
 */
export async function sha256Prefixed(bytes: Uint8Array): Promise<string> {
  return 'sha256:' + (await sha256Hex(bytes));
}

/**
 * Verify an Ed25519 signature (RFC 8032) over `message`. `publicKeyRaw` is the
 * 32-byte raw key; `signature` the 64-byte value. Returns false on a bad
 * signature or malformed key — never throws for those.
 */
export async function verifyEd25519(
  publicKeyRaw: Uint8Array,
  signature: Uint8Array,
  message: Uint8Array,
): Promise<boolean> {
  let key: CryptoKey;
  try {
    key = await subtle.importKey('raw', publicKeyRaw as BufferSource, { name: 'Ed25519' }, false, [
      'verify',
    ]);
  } catch {
    // A key that will not import cannot verify anything.
    return false;
  }
  try {
    return await subtle.verify({ name: 'Ed25519' }, key, signature as BufferSource, message as BufferSource);
  } catch {
    return false;
  }
}
