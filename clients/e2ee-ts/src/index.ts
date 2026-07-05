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

import { x25519 } from "@noble/curves/ed25519";
import { secp256k1 } from "@noble/curves/secp256k1";
import { bytesToHex, randomBytes, utf8ToBytes } from "@noble/hashes/utils";

import { sealSecp256k1, sealX25519 } from "./internal.ts";

/** X25519 cipher suite identifier (spec §7.1, RECOMMENDED). */
export const ALGO_X25519 = "x25519-aes-256-gcm-hkdf-sha256";
/** secp256k1 cipher suite identifier (spec §7.1). */
export const ALGO_SECP256K1 = "secp256k1-aes-256-gcm-hkdf-sha256";

const NONCE_LEN = 12;

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
  assertIntegerTimestamp(timestamp);
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
  assertIntegerTimestamp(timestamp);
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

/**
 * The AAD `ts` is a JSON integer (spec §7.3); the gateway's canonicalizer
 * rejects non-integer numbers, so a float would silently fail to decrypt.
 */
function assertIntegerTimestamp(timestamp: number): void {
  if (!Number.isInteger(timestamp)) {
    throw new Error("timestamp must be an integer number of Unix seconds");
  }
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
