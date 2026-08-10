/**
 * Receipt verification (§7, §9.3). A receipt is one JSON document; its
 * `signature` is Ed25519 over JCS(document minus `signature`) under a key
 * the established keyset lists. "Established" means a keyset whose digest
 * the caller verified — through {@link verifyReportBinding}, or published
 * by a party the client trusts (§9.3).
 */

import { verifyEd25519, sha256Prefixed, jcsBytes, fromHex } from './crypto.js';
import type {
  Check,
  ReceiptEnvelope,
  ReceiptEvent,
  ReceiptPayload,
  ReceiptVerification,
  WorkloadKeyset,
} from './types.js';

/**
 * §9.3 checks 1–2: the `signature` member verifies over JCS(document minus
 * `signature`) under the keyset entry `key_id` names, and the document's
 * `workload_keyset_digest` equals the established digest. Documents whose
 * `api_version` is not `aci/1` are rejected (Appendix B).
 *
 * Returns per-check results plus the document for the body-hash checks; a
 * failed check is `ok: false`, never thrown.
 */
export async function verifyReceipt(
  document: ReceiptEnvelope,
  keyset: WorkloadKeyset,
  establishedDigest: string,
): Promise<ReceiptVerification> {
  const checks: Check[] = [];

  // §9.3 check 1: Ed25519 over JCS(document minus `signature`).
  const signingKeys = Array.isArray(keyset.receipt_signing_keys)
    ? keyset.receipt_signing_keys
    : [];
  const keyEntry = signingKeys.find((k) => k.key_id === document.key_id);
  if (!keyEntry) {
    checks.push({
      name: 'signature',
      ok: false,
      detail: `key_id "${document.key_id}" not in receipt_signing_keys`,
    });
  } else if (keyEntry.algo !== 'ed25519') {
    // Appendix B: ed25519 is the only defined signature algorithm; reject others.
    checks.push({
      name: 'signature',
      ok: false,
      detail: `unsupported signature algo "${keyEntry.algo}"`,
    });
  } else {
    const { signature, ...unsigned } = document;
    let ok = false;
    try {
      ok = await verifyEd25519(
        fromHex(keyEntry.public_key),
        fromHex(signature),
        jcsBytes(unsigned),
      );
    } catch {
      // Malformed hex is a failed verification, not a thrown one.
    }
    checks.push({
      name: 'signature',
      ok,
      ...(ok ? {} : { detail: `ed25519 verification failed under "${document.key_id}"` }),
    });
  }

  const payload = document as unknown as ReceiptPayload;
  // Appendix B: reject receipts with a foreign api_version.
  const versionOk = payload.api_version === 'aci/1';
  checks.push({
    name: 'api_version',
    ok: versionOk,
    ...(versionOk ? {} : { detail: `api_version "${payload.api_version}" is not "aci/1"` }),
  });
  // §9.3 check 2: the document binds back to the established keyset.
  const ok = payload.workload_keyset_digest === establishedDigest;
  checks.push({
    name: 'workload_keyset_digest',
    ok,
    ...(ok
      ? {}
      : { detail: `document ${payload.workload_keyset_digest} != established ${establishedDigest}` }),
  });

  return {
    ok: checks.every((c) => c.ok),
    checks,
    payload,
  };
}

/** Find the first event of a given type in a receipt payload's event log. */
export function findEvent(payload: ReceiptPayload, type: string): ReceiptEvent | undefined {
  // Server-supplied JSON: a malformed document is a failed lookup, not a throw.
  if (!Array.isArray(payload.event_log)) return undefined;
  return payload.event_log.find((e) => e.type === type);
}

/**
 * `sha256:<hex>` of raw body bytes — the form ACI body hashes use (Appendix A). Accepts
 * a string (UTF-8 encoded) or raw bytes.
 */
export async function hashBody(body: Uint8Array | string): Promise<string> {
  const bytes = typeof body === 'string' ? new TextEncoder().encode(body) : body;
  return sha256Prefixed(bytes);
}

/**
 * §9.3 check 3: `request.received.body_hash` matches the plaintext wire body,
 * or the compact post-decryption JSON body for E2EE (§7.4). Returns false when
 * the event or its hash is absent.
 */
export async function checkRequestBodyHash(
  payload: ReceiptPayload,
  requestBody: Uint8Array | string,
): Promise<boolean> {
  return eventHashMatches(payload, 'request.received', requestBody);
}

/**
 * §9.3 check 4: `response.returned.body_hash` matches the response bytes this
 * client received off the wire — the in-order raw SSE bytes for a stream,
 * including encrypted E2EE field values (§7.4). Returns false when the event
 * or its hash is absent.
 */
export async function checkResponseBodyHash(
  payload: ReceiptPayload,
  responseBody: Uint8Array | string,
): Promise<boolean> {
  return eventHashMatches(payload, 'response.returned', responseBody);
}

async function eventHashMatches(
  payload: ReceiptPayload,
  type: string,
  body: Uint8Array | string,
): Promise<boolean> {
  const expected = findEvent(payload, type)?.body_hash;
  if (typeof expected !== 'string') return false;
  return (await hashBody(body)) === expected;
}
