/**
 * Attested-session helpers (§9, §10.3). A session is content-addressed: its id
 * is the SHA-256 of the exact served document bytes, and the signed receipt
 * commits to that id — there is no session signature.
 */

import { sha256Prefixed, fromBase64 } from './crypto.js';
import type { SessionRecord, SessionEvidence } from './types.js';

/** `session_id` (§9): `sha256:<hex>` over the exact served session document bytes. */
export async function computeSessionId(sessionBytes: Uint8Array): Promise<string> {
  return sha256Prefixed(sessionBytes);
}

/** Appendix A: reject session documents whose `api_version` is not `aci/1`. */
export function checkSessionApiVersion(record: Pick<SessionRecord, 'api_version'>): boolean {
  return record.api_version === 'aci/1';
}

/**
 * §10.3 step 4: `evidence.data` decodes and hashes to `evidence.digest`.
 * Returns false when the data URI is absent, malformed, or does not hash.
 */
export async function checkSessionEvidence(evidence: SessionEvidence): Promise<boolean> {
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
