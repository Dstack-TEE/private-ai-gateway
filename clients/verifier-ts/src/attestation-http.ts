import { AciError } from './errors.js';
import type { AttestationReport } from './types.js';

// Shared by verifyService and the Node/Bun bootstrap path. Allows JSON escaping
// of the 4 MiB compose and 1 MiB log; production responses are about 54 KB.
export const MAX_ATTESTATION_RESPONSE_BYTES = 32 * 1024 * 1024;

/** Bound response bytes before UTF-8 and JSON decoding. */
export async function readAttestationReport(response: Response): Promise<AttestationReport> {
  const tooLarge = () => new AciError('attestation response exceeds maximum byte size (32 MiB)');
  if (Number(response.headers.get('content-length')) > MAX_ATTESTATION_RESPONSE_BYTES) {
    await response.body?.cancel();
    throw tooLarge();
  }
  const reader = response.body?.getReader();
  if (!reader) throw new AciError('attestation response has no body');
  let bytes = new Uint8Array(8192);
  let size = 0;
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      const required = size + value.byteLength;
      if (required > MAX_ATTESTATION_RESPONSE_BYTES) {
        await reader.cancel();
        throw tooLarge();
      }
      if (required > bytes.length) {
        let capacity = bytes.length;
        while (capacity < required) {
          capacity = Math.min(capacity * 2, MAX_ATTESTATION_RESPONSE_BYTES);
        }
        const grown = new Uint8Array(capacity);
        grown.set(bytes.subarray(0, size));
        bytes = grown;
      }
      bytes.set(value, size);
      size = required;
    }
  } finally {
    reader.releaseLock();
  }
  return JSON.parse(new TextDecoder().decode(bytes.subarray(0, size))) as AttestationReport;
}
