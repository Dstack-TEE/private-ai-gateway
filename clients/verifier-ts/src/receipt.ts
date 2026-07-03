/**
 * Level 1 receipt verification (§10.2 checks 1–2) and helpers for the body-hash
 * checks (§10.2 checks 3–4). "Established identity and keyset" means a keyset the
 * caller already trusts — from a Level 2 report verification, or published by a
 * party the client trusts. The recomputed `workload_id` and keyset digest of
 * that keyset are the values the receipt must match.
 */

import { receiptSigningBytes, computeWorkloadId, computeKeysetDigest } from './digest.js';
import { verifySignature, sha256Prefixed } from './crypto.js';
import { fromHex } from './crypto.js';
import type { Receipt, ReceiptEvent, WorkloadKeyset, Check, ReceiptVerification } from './types.js';

/**
 * Verify a receipt against an established keyset — §10.2 checks 1 and 2:
 *
 * 1. `signature.key_id` names a key in the keyset's `receipt_signing_keys`,
 *    `signature.algo` matches that key, and the signature verifies over the
 *    §8.5 canonical bytes under that key.
 * 2. The receipt's `workload_id` and `workload_keyset_digest` equal the values
 *    recomputed from the established keyset (§4.1, §4.2).
 *
 * Returns a per-check result — a failed check is `ok: false`, never thrown.
 * Throws {@link UnsupportedAlgorithmError} only when the signing algorithm is
 * outside Web Crypto scope (e.g. `ecdsa-secp256k1`).
 */
export async function verifyReceipt(
  receipt: Receipt,
  keyset: WorkloadKeyset,
): Promise<ReceiptVerification> {
  const checks: Check[] = [];

  const establishedWorkloadId = await computeWorkloadId(keyset.workload_identity.public_key);
  const establishedDigest = await computeKeysetDigest(keyset);

  // Check 2: self-described identity matches the established keyset.
  checks.push({
    name: 'workload_id',
    ok: receipt.workload_id === establishedWorkloadId,
    ...(receipt.workload_id === establishedWorkloadId
      ? {}
      : { detail: `receipt ${receipt.workload_id} != established ${establishedWorkloadId}` }),
  });
  checks.push({
    name: 'workload_keyset_digest',
    ok: receipt.workload_keyset_digest === establishedDigest,
    ...(receipt.workload_keyset_digest === establishedDigest
      ? {}
      : { detail: `receipt ${receipt.workload_keyset_digest} != established ${establishedDigest}` }),
  });

  // Check 1: signature under a named receipt signing key.
  const keyEntry = keyset.receipt_signing_keys.find((k) => k.key_id === receipt.signature.key_id);
  if (!keyEntry) {
    checks.push({
      name: 'signature',
      ok: false,
      detail: `signature.key_id "${receipt.signature.key_id}" not in receipt_signing_keys`,
    });
  } else if (receipt.signature.algo !== keyEntry.algo) {
    // §3.1: the attested key decides the algorithm; the receipt may not override it.
    checks.push({
      name: 'signature',
      ok: false,
      detail: `signature.algo "${receipt.signature.algo}" != keyset entry algo "${keyEntry.algo}"`,
    });
  } else {
    const message = receiptSigningBytes(receipt);
    const ok = await verifySignature(
      keyEntry.algo,
      fromHex(keyEntry.public_key),
      fromHex(receipt.signature.value),
      message,
      'receipt signature (§8.5)',
    );
    checks.push({ name: 'signature', ok, ...(ok ? {} : { detail: 'Ed25519 verification failed' }) });
  }

  return { ok: checks.every((c) => c.ok), checks };
}

/** Find the first event of a given type in a receipt's event log. */
export function findEvent(receipt: Receipt, type: string): ReceiptEvent | undefined {
  return receipt.event_log.find((e) => e.type === type);
}

/**
 * `sha256:<hex>` of raw body bytes — the form ACI body hashes use (§3). Accepts a
 * string (UTF-8 encoded) or raw bytes.
 */
export async function hashBody(body: Uint8Array | string): Promise<string> {
  const bytes = typeof body === 'string' ? new TextEncoder().encode(body) : body;
  return sha256Prefixed(bytes);
}

/**
 * §10.2 check 3: the request bytes the client sent match `request.received.body_hash`.
 * For E2EE requests, pass the decrypted body as the service observed it (§8.3, §12).
 * Returns false when the event or its hash is absent.
 */
export async function checkRequestBodyHash(
  receipt: Receipt,
  requestBody: Uint8Array | string,
): Promise<boolean> {
  const event = findEvent(receipt, 'request.received');
  const expected = event?.body_hash;
  if (typeof expected !== 'string') return false;
  return (await hashBody(requestBody)) === expected;
}

/**
 * §10.2 check 4: the response bytes the client received match
 * `response.returned.wire_hash` — for a stream, the in-order raw SSE bytes.
 * Returns false when the event or its hash is absent.
 */
export async function checkResponseWireHash(
  receipt: Receipt,
  responseBody: Uint8Array | string,
): Promise<boolean> {
  const event = findEvent(receipt, 'response.returned');
  const expected = event?.wire_hash;
  if (typeof expected !== 'string') return false;
  return (await hashBody(responseBody)) === expected;
}

/**
 * For E2EE responses, check the decrypted response bytes match
 * `response.returned.cleartext_hash` (§10.2 check 4, §12). Only meaningful when
 * the client can reproduce the service's pre-encryption serialization.
 */
export async function checkResponseCleartextHash(
  receipt: Receipt,
  cleartextBody: Uint8Array | string,
): Promise<boolean> {
  const event = findEvent(receipt, 'response.returned');
  const expected = event?.cleartext_hash;
  if (typeof expected !== 'string') return false;
  return (await hashBody(cleartextBody)) === expected;
}
