/**
 * Cryptographic primitives, all via the Web Crypto API (`globalThis.crypto`) so
 * the same code runs in browsers and in Node 20+ with no third-party deps.
 * Only SHA-256 and Ed25519 verification are needed for Level 1.
 */

import { AciFormatError, UnsupportedAlgorithmError } from './errors.js';

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
    const byte = Number.parseInt(h.substr(i * 2, 2), 16);
    if (Number.isNaN(byte)) {
      throw new AciFormatError(`invalid hex at offset ${i * 2}: "${h.substr(i * 2, 2)}"`);
    }
    out[i] = byte;
  }
  return out;
}

/** SHA-256 of the given bytes. */
export async function sha256(bytes: Uint8Array): Promise<Uint8Array> {
  return new Uint8Array(await subtle.digest('SHA-256', bytes as BufferSource));
}

/** Lowercase-hex SHA-256 of the given bytes. */
export async function sha256Hex(bytes: Uint8Array): Promise<string> {
  return toHex(await sha256(bytes));
}

/**
 * `sha256:<lowercase-hex>` digest string of the given bytes — the ACI digest
 * form (§3) used for `workload_id`, keyset digests, and body hashes.
 */
export async function sha256Prefixed(bytes: Uint8Array): Promise<string> {
  return 'sha256:' + (await sha256Hex(bytes));
}

/**
 * Verify an Ed25519 signature (RFC 8032, §4.3/§8.5) over `message`.
 * `publicKeyRaw` is the 32-byte raw key; `signature` the 64-byte value.
 * Returns false on a bad signature or malformed key — never throws for those.
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

/**
 * Verify a signature by ACI signature `algo`, dispatching on the algorithm the
 * attested keyset entry declares. Only `ed25519` is verifiable here; every other
 * algorithm (including `ecdsa-secp256k1`) raises {@link UnsupportedAlgorithmError}.
 */
export async function verifySignature(
  algo: string,
  publicKeyRaw: Uint8Array,
  signature: Uint8Array,
  message: Uint8Array,
  context: string,
): Promise<boolean> {
  if (algo === 'ed25519') {
    return verifyEd25519(publicKeyRaw, signature, message);
  }
  throw new UnsupportedAlgorithmError(algo, context);
}
