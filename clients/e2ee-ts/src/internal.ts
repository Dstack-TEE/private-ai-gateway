// Deterministic AES-GCM seals — the caller supplies the ephemeral key and GCM
// nonce. These are NOT part of the public API and are NOT re-exported from
// index.ts: a public function taking a caller-chosen GCM nonce is a nonce-reuse
// footgun (AES-GCM breaks catastrophically under nonce reuse). The public
// `encrypt` generates a fresh ephemeral key and nonce per call. The test suite
// imports these directly for the deterministic known-answer vectors.

import { gcm } from "@noble/ciphers/aes";
import { x25519 } from "@noble/curves/ed25519";
import { secp256k1 } from "@noble/curves/secp256k1";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { bytesToHex, concatBytes, hexToBytes, utf8ToBytes } from "@noble/hashes/utils";

const HKDF_INFO_X25519 = "aci.e2ee.v2.x25519";
const HKDF_INFO_SECP256K1 = "aci.e2ee.v2.secp256k1";
// HKDF salt "none" (spec §7.1). Empty and 32-zero salts yield the same
// HMAC-SHA256 PRK, so this matches the Rust `Hkdf::new(None, ..)` client.
const EMPTY_SALT = new Uint8Array(0);

export function sealX25519(
  recipientHex: string,
  ephemeralSecret: Uint8Array,
  gcmNonce: Uint8Array,
  plaintext: Uint8Array,
  aad: Uint8Array,
): string {
  const recipient = parseX25519Public(recipientHex);
  const ephemeralPublic = x25519.getPublicKey(ephemeralSecret);
  const shared = x25519.getSharedSecret(ephemeralSecret, recipient);
  const key = hkdf(sha256, shared, EMPTY_SALT, utf8ToBytes(HKDF_INFO_X25519), 32);
  const ciphertext = gcm(key, gcmNonce, aad).encrypt(plaintext);
  return bytesToHex(concatBytes(ephemeralPublic, gcmNonce, ciphertext));
}

export function sealSecp256k1(
  recipientHex: string,
  ephemeralSecret: Uint8Array,
  gcmNonce: Uint8Array,
  plaintext: Uint8Array,
  aad: Uint8Array,
): string {
  const recipient = parseSecp256k1Public(recipientHex);
  const ephemeralPublic = secp256k1.getPublicKey(ephemeralSecret, false);
  // Shared secret is the x-coordinate of the shared point (spec §7.1): take the
  // 32 bytes after the 0x02/0x03 prefix of the compressed encoding.
  const sharedX = secp256k1.getSharedSecret(ephemeralSecret, recipient, true).slice(1);
  const key = hkdf(sha256, sharedX, EMPTY_SALT, utf8ToBytes(HKDF_INFO_SECP256K1), 32);
  const ciphertext = gcm(key, gcmNonce, aad).encrypt(plaintext);
  return bytesToHex(concatBytes(ephemeralPublic, gcmNonce, ciphertext));
}

function parseX25519Public(value: string): Uint8Array {
  const bytes = decodeHex(value);
  if (bytes.length !== 32) {
    throw new Error(`invalid public key: X25519 key must be 32 bytes, got ${bytes.length}`);
  }
  return bytes;
}

function parseSecp256k1Public(value: string): Uint8Array {
  const bytes = decodeHex(value);
  // Accept the 65-byte uncompressed SEC1 form and the 64-byte form without the
  // 0x04 prefix (spec §7.1).
  if (bytes.length === 65) {
    if (bytes[0] !== 0x04) {
      throw new Error("invalid public key: 65-byte secp256k1 key must start with 0x04");
    }
    return bytes;
  }
  if (bytes.length === 64) return concatBytes(new Uint8Array([0x04]), bytes);
  throw new Error(`invalid public key: secp256k1 key must be 64 or 65 bytes, got ${bytes.length}`);
}

function decodeHex(value: string): Uint8Array {
  return hexToBytes(value.startsWith("0x") ? value.slice(2) : value);
}
