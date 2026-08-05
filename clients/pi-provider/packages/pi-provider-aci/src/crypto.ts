// Receipt signature verification and hashing helpers, mirroring the gateway's
// `src/aci/keys.rs`.
//
// Receipt signatures are verified with **ed25519 only** (ACI §8.5):
//   - `ed25519` — 64-byte RFC 8032 signature over the JCS canonical receipt
//     with `signature.value` omitted. Deterministic, standard-library
//     verifiable. This is the algorithm the live gateway uses
//     (`dstack-kms-receipt-ed25519-v1`).
//
// Legacy `ecdsa-secp256k1` *recoverable* receipt signatures (over
// sha256(canonical_bytes)) are intentionally not implemented; the gateway no
// longer issues them.
//
// NOTE: secp256k1 is still used for the attestation *keyset endorsement*
// (the live gateway signs workload_keyset endorsements with
// `ecdsa-secp256k1`); that verification lives in verify.ts and relies on the
// noble synchronous hash wiring set up below.
//
// (Per-field E2EE request encryption was removed in favor of attested TLS SPKI
// pinning; see src/tls-pinning.ts.)

import { hashes } from "@noble/secp256k1";
import { sha256 as nobleSha256 } from "@noble/hashes/sha2.js";
import { hmac } from "@noble/hashes/hmac.js";
import { hexToBytes, bytesToHex, concatBytes } from "@noble/hashes/utils.js";
import { createPublicKey, verify as nodeVerify } from "node:crypto";

// noble/secp256k1 v3 expects synchronous hash/HMAC providers on the `hashes`
// object. Wire them once at module load (needed by the keyset-endorsement
// secp256k1 verification in verify.ts).
if (!hashes.sha256) {
  hashes.sha256 = nobleSha256 as never;
}
if (!hashes.hmacSha256) {
  hashes.hmacSha256 = (key: Uint8Array, ...messages: Uint8Array[]) =>
    hmac(nobleSha256, key, concatBytes(...messages)) as never;
}

// ----------------------------------------------------------------------------
// ed25519 receipt signature verification (§8.5)
// ----------------------------------------------------------------------------

const ED25519_SPKI_PREFIX = Buffer.from("302a300506032b6570032100", "hex");

/**
 * Verify an ACI ed25519 receipt signature (§8.5).
 * @param publicKeyHex 32-byte raw ed25519 public key hex from
 *   attestation.workload_keyset.receipt_signing_keys[].public_key
 * @param canonicalBytes JCS bytes of the receipt with signature.value omitted
 * @param signature 64-byte RFC 8032 signature
 */
export function verifyReceiptSignatureEd25519(
  publicKeyHex: string,
  canonicalBytes: Uint8Array,
  signature: Uint8Array,
): boolean {
  try {
    const pubRaw = hexToBytes(publicKeyHex);
    if (pubRaw.length !== 32) return false;
    if (signature.length !== 64) return false;
    // Wrap the raw 32-byte key into an SPKI structure for Node's crypto layer.
    const spki = new Uint8Array(ED25519_SPKI_PREFIX.length + pubRaw.length);
    spki.set(ED25519_SPKI_PREFIX, 0);
    spki.set(pubRaw, ED25519_SPKI_PREFIX.length);
    const key = createPublicKey({ key: Buffer.from(spki), format: "der", type: "spki" });
    return nodeVerify(null, canonicalBytes, key, signature);
  } catch {
    return false;
  }
}

// ----------------------------------------------------------------------------
// Hash helpers
// ----------------------------------------------------------------------------

/** `"sha256:" + hex(sha256(payload))`. Matches gateway `sha256_hex`. */
export function sha256Hex(payload: Uint8Array): string {
  return `sha256:${bytesToHex(nobleSha256(payload))}`;
}