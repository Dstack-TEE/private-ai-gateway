/**
 * Receipt verification (§8, §10.2). The envelope serves the payload as exact
 * bytes (`payload_b64`); the signature is Ed25519 over those decoded bytes
 * under a key the established keyset lists. "Established" means a keyset whose
 * digest the caller verified — through {@link verifyReportBinding}, or
 * published by a party the client trusts (Level 1, §10).
 */

import { verifyEd25519, sha256Prefixed, fromBase64, fromHex } from './crypto.js';
import type {
  Check,
  ReceiptEnvelope,
  ReceiptEvent,
  ReceiptPayload,
  ReceiptVerification,
  WorkloadKeyset,
} from './types.js';

/**
 * §10.2 checks 1–2: the envelope signature verifies over the decoded payload
 * bytes under the keyset entry `key_id` names (whose `algo` the envelope must
 * match — the attested entry decides the algorithm, §3), and the payload's
 * `workload_keyset_digest` equals the established digest. Payloads whose
 * `api_version` is not `aci/1` are rejected (Appendix A).
 *
 * Returns per-check results and the parsed payload for the body-hash checks;
 * a failed check is `ok: false`, never thrown.
 */
export async function verifyReceipt(
  envelope: ReceiptEnvelope,
  keyset: WorkloadKeyset,
  establishedDigest: string,
): Promise<ReceiptVerification> {
  const checks: Check[] = [];

  let payloadBytes: Uint8Array | undefined;
  try {
    payloadBytes = fromBase64(envelope.payload_b64);
  } catch {
    // Handled below: without the payload bytes neither check can run.
  }
  if (payloadBytes === undefined) {
    const detail = 'payload_b64 does not decode as base64';
    checks.push({ name: 'signature', ok: false, detail });
    checks.push({ name: 'workload_keyset_digest', ok: false, detail });
    return { ok: false, checks };
  }

  // §10.2 check 1: Ed25519 over the served payload bytes.
  const keyEntry = keyset.receipt_signing_keys.find((k) => k.key_id === envelope.key_id);
  if (!keyEntry) {
    checks.push({
      name: 'signature',
      ok: false,
      detail: `key_id "${envelope.key_id}" not in receipt_signing_keys`,
    });
  } else if (envelope.algo !== keyEntry.algo) {
    checks.push({
      name: 'signature',
      ok: false,
      detail: `envelope algo "${envelope.algo}" != keyset entry algo "${keyEntry.algo}"`,
    });
  } else if (keyEntry.algo !== 'ed25519') {
    // Appendix A: ed25519 is the only defined signature algorithm; reject others.
    checks.push({
      name: 'signature',
      ok: false,
      detail: `unsupported signature algo "${keyEntry.algo}"`,
    });
  } else {
    let ok = false;
    try {
      ok = await verifyEd25519(
        fromHex(keyEntry.public_key),
        fromHex(envelope.signature),
        payloadBytes,
      );
    } catch {
      // Malformed hex is a failed verification, not a thrown one.
    }
    checks.push({
      name: 'signature',
      ok,
      ...(ok ? {} : { detail: `ed25519 verification failed under "${envelope.key_id}"` }),
    });
  }

  // §10.2 check 2: the payload binds back to the established keyset.
  let payload: ReceiptPayload | undefined;
  try {
    payload = JSON.parse(new TextDecoder().decode(payloadBytes)) as ReceiptPayload;
  } catch {
    // Handled below.
  }
  if (payload === undefined) {
    checks.push({
      name: 'workload_keyset_digest',
      ok: false,
      detail: 'payload bytes are not valid JSON',
    });
  } else {
    // Appendix A: reject receipt payloads with a foreign api_version.
    const versionOk = payload.api_version === 'aci/1';
    checks.push({
      name: 'api_version',
      ok: versionOk,
      ...(versionOk ? {} : { detail: `payload api_version "${payload.api_version}" is not "aci/1"` }),
    });
    const ok = payload.workload_keyset_digest === establishedDigest;
    checks.push({
      name: 'workload_keyset_digest',
      ok,
      ...(ok
        ? {}
        : { detail: `payload ${payload.workload_keyset_digest} != established ${establishedDigest}` }),
    });
  }

  return {
    ok: checks.every((c) => c.ok),
    checks,
    ...(payload !== undefined ? { payload } : {}),
  };
}

/** Find the first event of a given type in a receipt payload's event log. */
export function findEvent(payload: ReceiptPayload, type: string): ReceiptEvent | undefined {
  return payload.event_log.find((e) => e.type === type);
}

/**
 * `sha256:<hex>` of raw body bytes — the form ACI body hashes use (§3). Accepts
 * a string (UTF-8 encoded) or raw bytes.
 */
export async function hashBody(body: Uint8Array | string): Promise<string> {
  const bytes = typeof body === 'string' ? new TextEncoder().encode(body) : body;
  return sha256Prefixed(bytes);
}

/**
 * §10.2 check 3: `request.received.body_hash` matches the request bytes this
 * client sent — the wire body for plaintext, the original body it sealed for
 * E2EE (§8.4). Returns false when the event or its hash is absent.
 */
export async function checkRequestBodyHash(
  payload: ReceiptPayload,
  requestBody: Uint8Array | string,
): Promise<boolean> {
  return eventHashMatches(payload, 'request.received', requestBody);
}

/**
 * §10.2 check 4: `response.returned.body_hash` matches the response bytes this
 * client received off the wire — the in-order raw SSE bytes for a stream, the
 * sealed envelope bytes for E2EE (§8.4). Returns false when the event or its
 * hash is absent.
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
