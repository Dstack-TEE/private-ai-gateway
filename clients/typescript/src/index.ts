// Client-side ACI end-to-end encryption (ACI spec §7).
//
// An ACI client encrypts each content-bearing request field to the attested
// service E2EE key, so the plaintext is readable only inside the TEE even when
// TLS terminates elsewhere. This module produces the wire ciphertext for one
// field and the AES-GCM associated data (AAD) that binds it to its location
// and request context.
//
// Both cipher suites of §7.1 are supported; select one by the `algo` of the
// keyset entry you encrypt to:
//   * ALGO_X25519   — X25519 + HKDF-SHA256 + AES-256-GCM (RECOMMENDED)
//   * ALGO_SECP256K1 — secp256k1 + HKDF-SHA256 + AES-256-GCM
//
// Each field value on the wire is the lowercase hex of
//   ephemeral_public_key || aes_gcm_nonce(12) || ciphertext || tag(16)
// with a fresh ephemeral key and GCM nonce per field.

import { gcm } from "@noble/ciphers/aes";
import { x25519 } from "@noble/curves/ed25519";
import { secp256k1 } from "@noble/curves/secp256k1";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import {
  bytesToHex,
  concatBytes,
  hexToBytes,
  randomBytes,
  utf8ToBytes,
} from "@noble/hashes/utils";

/** X25519 cipher suite identifier (spec §7.1, RECOMMENDED). */
export const ALGO_X25519 = "x25519-aes-256-gcm-hkdf-sha256";
/** secp256k1 cipher suite identifier (spec §7.1). */
export const ALGO_SECP256K1 = "secp256k1-aes-256-gcm-hkdf-sha256";

const HKDF_INFO_X25519 = "aci.e2ee.v2.x25519";
const HKDF_INFO_SECP256K1 = "aci.e2ee.v2.secp256k1";
const NONCE_LEN = 12;
// HKDF salt "none" (spec §7.1). Empty and 32-zero salts yield the same
// HMAC-SHA256 PRK, so this matches the Rust `Hkdf::new(None, ..)` client.
const EMPTY_SALT = new Uint8Array(0);

/**
 * Encrypt one request field and return its wire ciphertext (lowercase hex).
 *
 * `field` is the location's field path (spec §7.2), e.g. `messages.0.content`,
 * `messages.1.content.0.image_url.url`, `prompt`, or `input.2`. `model` is the
 * request's top-level `model`, byte-exact. `nonce` / `timestamp` are the
 * `X-E2EE-Nonce` / `X-E2EE-Timestamp` you send with the request.
 */
export function encryptRequestField(
  servicePublicKeyHex: string,
  algo: string,
  model: string,
  field: string,
  nonce: string,
  timestamp: number,
  plaintext: Uint8Array,
): string {
  const aad = requestAad(algo, model, field, nonce, timestamp);
  return encrypt(servicePublicKeyHex, algo, plaintext, aad);
}

/**
 * Encrypt `plaintext` to `servicePublicKeyHex` under `algo` with the given
 * `aad`. Use {@link requestAad} / {@link responseAad} to build `aad`, or
 * {@link encryptRequestField} for the common request case.
 */
export function encrypt(
  servicePublicKeyHex: string,
  algo: string,
  plaintext: Uint8Array,
  aad: Uint8Array,
): string {
  const gcmNonce = randomBytes(NONCE_LEN);
  if (algo === ALGO_X25519) {
    return sealX25519(servicePublicKeyHex, x25519.utils.randomPrivateKey(), gcmNonce, plaintext, aad);
  }
  if (algo === ALGO_SECP256K1) {
    return sealSecp256k1(servicePublicKeyHex, secp256k1.utils.randomPrivateKey(), gcmNonce, plaintext, aad);
  }
  throw new Error(`unsupported E2EE algo: ${algo}`);
}

/**
 * The request-field AAD (spec §7.3): the JCS canonicalization of the
 * purpose-tagged object bound into AES-GCM.
 */
export function requestAad(
  algo: string,
  model: string,
  field: string,
  nonce: string,
  timestamp: number,
): Uint8Array {
  return canonicalObject({
    purpose: "aci.e2ee.request.v2",
    algo,
    model,
    field,
    nonce,
    ts: timestamp,
  });
}

/**
 * The response-field AAD (spec §7.3): like {@link requestAad} but tagged
 * `aci.e2ee.response.v2` and additionally binding the response `id` (`""` when
 * the response carries none). Use the values from your own request for `algo`
 * / `model` / `nonce` / `timestamp`; the service derives the same AAD.
 */
export function responseAad(
  algo: string,
  model: string,
  id: string,
  field: string,
  nonce: string,
  timestamp: number,
): Uint8Array {
  return canonicalObject({
    purpose: "aci.e2ee.response.v2",
    algo,
    model,
    id,
    field,
    nonce,
    ts: timestamp,
  });
}

/** Generate a fresh `X-E2EE-Nonce`: 32 CSPRNG bytes as 64 lowercase hex characters (spec §7.5). */
export function generateNonce(): string {
  return bytesToHex(randomBytes(32));
}

// --- internals ------------------------------------------------------------

// Exported for the deterministic known-answer test; not part of the public API.
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

// Exported for the deterministic known-answer test; not part of the public API.
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
  if (bytes.length === 65) return bytes;
  if (bytes.length === 64) return concatBytes(new Uint8Array([0x04]), bytes);
  throw new Error(`invalid public key: secp256k1 key must be 64 or 65 bytes, got ${bytes.length}`);
}

function decodeHex(value: string): Uint8Array {
  return hexToBytes(value.startsWith("0x") ? value.slice(2) : value);
}

/**
 * JCS canonicalization (RFC 8785) of a flat object of string / integer
 * scalars — the subset ACI AADs use. Keys are sorted by UTF-16 code unit
 * (JavaScript's default string order, which is what JCS §3.2.3 specifies), and
 * `JSON.stringify` supplies RFC 8785-compatible string escaping and integer
 * formatting. We sort explicitly rather than trusting object key order.
 */
function canonicalObject(obj: Record<string, string | number>): Uint8Array {
  const keys = Object.keys(obj).sort();
  const body = keys.map((k) => `${JSON.stringify(k)}:${JSON.stringify(obj[k])}`).join(",");
  return utf8ToBytes(`{${body}}`);
}
