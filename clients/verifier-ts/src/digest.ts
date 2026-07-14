/**
 * The ACI digest constructions (§3, §4.1, §4.2). Artifacts the service builds
 * are hashed as the exact served bytes; the attestation statement is the one
 * report payload a verifier constructs itself, as a fixed byte template whose
 * inputs are restricted so no JSON escaping is ever needed.
 */

import { sha256Hex, sha256Prefixed } from './crypto.js';
import { AciFormatError } from './errors.js';

const DIGEST_RE = /^sha256:[0-9a-f]{64}$/;
const NONCE_RE = /^[0-9A-Za-z_-]{1,128}$/;

/** `workload_keyset_digest` (§4.1) over the base64-decoded `workload_keyset_b64` bytes. */
export async function computeKeysetDigest(keysetBytes: Uint8Array): Promise<string> {
  return sha256Prefixed(keysetBytes);
}

/**
 * The exact attestation-statement bytes (§4.2) for a keyset digest and the
 * nonce the client sent — `null`/`undefined` when the query parameter was
 * omitted, which puts the JSON literal `null` in the template. Inputs outside
 * the spec-pinned formats throw {@link AciFormatError}.
 */
export function attestationStatement(
  keysetDigest: string,
  nonce: string | null | undefined,
): Uint8Array {
  if (!DIGEST_RE.test(keysetDigest)) {
    throw new AciFormatError(`keyset digest is not sha256:<64-hex>: "${keysetDigest}"`);
  }
  if (nonce != null && !NONCE_RE.test(nonce)) {
    throw new AciFormatError('nonce must be 1-128 chars of [0-9A-Za-z_-] (§4.2)');
  }
  const noncePart = nonce == null ? 'null' : `"${nonce}"`;
  return new TextEncoder().encode(
    `{"keyset_digest":"${keysetDigest}","nonce":${noncePart},"purpose":"aci.report_data.v1"}`,
  );
}

/**
 * `report_data` (§4.2): SHA-256 of the attestation statement, as bare lowercase
 * hex (it fills a report-data slot, not an ACI digest string). The TEE places
 * these 32 bytes zero-padded to 64 in the quote's report-data field.
 */
export async function computeReportData(
  keysetDigest: string,
  nonce: string | null | undefined,
): Promise<string> {
  return sha256Hex(attestationStatement(keysetDigest, nonce));
}
