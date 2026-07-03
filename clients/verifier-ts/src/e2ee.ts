/**
 * E2EE associated-data (AAD) builders (§7.3). The AAD binds each ciphertext to
 * its field path and request context; it is the JCS of a purpose-tagged object,
 * so no component needs bespoke escaping. Provided here because clients that
 * encrypt fields need the exact bytes — the verifier itself does not decrypt.
 * The X25519 and secp256k1 suites share these builders; `algo` is just a field.
 */

import { canonicalize, jcsBytes } from './jcs.js';

/** Inputs shared by request and response AAD (§7.3). */
export interface AadCommon {
  /** `algo` of the selected service E2EE key. */
  algo: string;
  /** The request's top-level `model`, byte-exact. */
  model: string;
  /** The encrypted location's field path, e.g. `messages.0.content` (§7.2). */
  field: string;
  /** The request's `X-E2EE-Nonce` (string). */
  nonce: string;
  /** The request's `X-E2EE-Timestamp` (Unix seconds, integer). */
  ts: number;
}

/** Request AAD (§7.3), tag `aci.e2ee.request.v2`. Returns the canonical JCS string. */
export function requestAadString(params: AadCommon): string {
  return canonicalize({
    purpose: 'aci.e2ee.request.v2',
    algo: params.algo,
    model: params.model,
    field: params.field,
    nonce: params.nonce,
    ts: params.ts,
  });
}

/** Request AAD as UTF-8 bytes — the value passed to AES-GCM. */
export function requestAad(params: AadCommon): Uint8Array {
  return jcsBytes({
    purpose: 'aci.e2ee.request.v2',
    algo: params.algo,
    model: params.model,
    field: params.field,
    nonce: params.nonce,
    ts: params.ts,
  });
}

/** Response AAD (§7.3), tag `aci.e2ee.response.v2`. Adds the response `id` (`""` when none). */
export function responseAadString(params: AadCommon & { id: string }): string {
  return canonicalize({
    purpose: 'aci.e2ee.response.v2',
    algo: params.algo,
    model: params.model,
    id: params.id,
    field: params.field,
    nonce: params.nonce,
    ts: params.ts,
  });
}

/** Response AAD as UTF-8 bytes — the value passed to AES-GCM. */
export function responseAad(params: AadCommon & { id: string }): Uint8Array {
  return jcsBytes({
    purpose: 'aci.e2ee.response.v2',
    algo: params.algo,
    model: params.model,
    id: params.id,
    field: params.field,
    nonce: params.nonce,
    ts: params.ts,
  });
}
