/**
 * Attested-session helpers (§8, §9.3). A session is content-addressed: its id
 * is the SHA-256 of the exact served document bytes, and the signed receipt
 * commits to that id — there is no session signature.
 */

import { sha256Hex, sha256Prefixed, jcsBytes, fromBase64 } from './crypto.js';
import type { SessionRecord, SessionEvidence } from './types.js';

/** `session_id` (§8): bare 64-hex sha256 of the JCS form of the parsed document. */
export async function computeSessionId(record: unknown): Promise<string> {
  return sha256Hex(jcsBytes(record));
}

/** Appendix B: reject session documents whose `api_version` is not `aci/1`. */
export function checkSessionApiVersion(record: Pick<SessionRecord, 'api_version'>): boolean {
  return record.api_version === 'aci/1';
}

/**
 * §9.2(2): `evidence.data` decodes and hashes to `evidence.digest`.
 * Returns false when the data URI is absent, malformed, or does not hash.
 */
export async function checkSessionEvidence(evidence: SessionEvidence): Promise<boolean> {
  if (evidence == null || typeof evidence !== 'object') return false;
  const { digest, data } = evidence;
  if (typeof digest !== 'string' || typeof data !== 'string') return false;
  const comma = data.indexOf(',');
  if (!data.startsWith('data:') || comma < 0 || !data.slice(0, comma).endsWith(';base64')) {
    return false;
  }
  let bytes: Uint8Array;
  try {
    bytes = fromBase64(data.slice(comma + 1));
  } catch {
    return false;
  }
  return (await sha256Prefixed(bytes)) === digest;
}
