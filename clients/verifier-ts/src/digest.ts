/**
 * The ACI digest and canonical-signing-bytes constructions (§4.1, §4.2, §4.3,
 * §4.4, §4.7, §8.5, §9.2). Each returns the exact bytes/strings the spec pins in
 * spec/test-vectors.md, so they double as the byte-for-byte reference.
 */

import { jcsBytes } from './jcs.js';
import type { JcsValue } from './jcs.js';
import { sha256Hex, sha256Prefixed } from './crypto.js';
import type { PublicKey, WorkloadKeyset, Receipt, SessionRecord } from './types.js';

/**
 * `workload_id` — the stable name of a workload (§4.1):
 * `"sha256:" || hex(sha256(JCS(public_key)))`.
 */
export async function computeWorkloadId(publicKey: PublicKey): Promise<string> {
  return sha256Prefixed(jcsBytes({ algo: publicKey.algo, public_key: publicKey.public_key }));
}

/**
 * `workload_keyset_digest` (§4.2): `"sha256:" || hex(sha256(JCS(keyset)))`,
 * over the whole keyset object as given.
 */
export async function computeKeysetDigest(keyset: WorkloadKeyset): Promise<string> {
  return sha256Prefixed(jcsBytes(keyset as JcsValue));
}

/**
 * The attestation statement (§4.4) whose JCS is hashed into `report_data`.
 * `nonce` is the request's decoded value, or JSON `null` when the query
 * parameter was omitted (never the string `"null"`); pass `undefined`/`null` for
 * the omitted case.
 */
export function attestationStatement(
  workloadId: string,
  workloadKeysetDigest: string,
  nonce: string | null | undefined,
): JcsValue {
  return {
    purpose: 'aci.report_data.v1',
    workload_id: workloadId,
    workload_keyset_digest: workloadKeysetDigest,
    nonce: nonce ?? null,
  };
}

/**
 * `report_data` (§4.4): `hex(sha256(JCS(attestation_statement)))` — the raw
 * 32-byte digest as lowercase hex, with no `sha256:` prefix (it names a bare
 * report-data slot, not an ACI digest string).
 */
export async function computeReportData(
  workloadId: string,
  workloadKeysetDigest: string,
  nonce: string | null | undefined,
): Promise<string> {
  return sha256Hex(jcsBytes(attestationStatement(workloadId, workloadKeysetDigest, nonce)));
}

/** JCS bytes of the keyset endorsement payload (§4.3), signed by the identity key. */
export function keysetEndorsementPayload(workloadKeysetDigest: string): Uint8Array {
  return jcsBytes({
    purpose: 'aci.keyset.endorsement.v1',
    workload_keyset_digest: workloadKeysetDigest,
  });
}

/** JCS bytes of the keyset revocation payload (§4.7), signed by the identity key. */
export function keysetRevocationPayload(workloadKeysetDigest: string): Uint8Array {
  return jcsBytes({
    purpose: 'aci.keyset.revocation.v1',
    workload_keyset_digest: workloadKeysetDigest,
  });
}

/**
 * Canonical bytes a receipt signature covers (§8.5): the JCS of the whole
 * receipt with only `signature.value` removed (`algo` and `key_id`, and any
 * other signature fields, are retained). Unknown top-level fields and events are
 * preserved by canonicalizing the object as given (§3.2).
 */
export function receiptSigningBytes(receipt: Receipt): Uint8Array {
  const { value: _omitted, ...signatureWithoutValue } = receipt.signature;
  const forSigning: JcsValue = {
    ...(receipt as unknown as { [k: string]: JcsValue }),
    signature: signatureWithoutValue as unknown as JcsValue,
  };
  return jcsBytes(forSigning);
}

/**
 * The content-addressing material for a session id (§9.2). The wire record omits
 * absent optional fields; the material restores `endpoint`, `identity`, and
 * `evidence.digest` as JSON `null`, and timestamps / raw evidence bytes are
 * excluded entirely.
 */
export function sessionMaterial(record: SessionRecord): JcsValue {
  return {
    upstream_name: record.upstream_name,
    endpoint: record.endpoint ?? null,
    verifier_id: record.verifier_id,
    identity: record.identity ?? null,
    channel_binding: record.channel_binding,
    claims: record.claims,
    evidence_digest: record.evidence?.digest ?? null,
  };
}

/**
 * `session_id` (§9.2): `"as_" || hex(sha256(JCS(material)))`. Recomputing this
 * from a fetched record and comparing it to the id the signed receipt committed
 * to is what makes the session tamper-evident — there is no session signature.
 */
export async function computeSessionId(record: SessionRecord): Promise<string> {
  return 'as_' + (await sha256Hex(jcsBytes(sessionMaterial(record))));
}
