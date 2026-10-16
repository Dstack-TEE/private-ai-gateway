import { AciError } from './errors.js';

// Shared by attestation, receipt and session fetches. Allows JSON escaping of
// the 4 MiB compose and 1 MiB log; production reports are about 54 KB and the
// largest of 21 measured production sessions was 1,428,775 bytes.
export const MAX_ATTESTATION_RESPONSE_BYTES = 32 * 1024 * 1024;
export const DEFAULT_TIMEOUT_MS = 10_000;

/** One deadline for fetching headers and consuming the response body. */
export async function timedFetch<T>(
  fetchImpl: (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>,
  input: RequestInfo | URL,
  init: RequestInit,
  timeoutMs: number,
  consume: (response: Response, signal: AbortSignal) => Promise<T>,
): Promise<T> {
  if (!Number.isFinite(timeoutMs) || timeoutMs <= 0) {
    throw new AciError('timeoutMs must be a positive number');
  }
  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(new AciError('ACI request timed out')), timeoutMs);
  timeout.unref?.();
  try {
    const response = await fetchImpl(input, { ...init, signal: controller.signal });
    return await consume(response, controller.signal);
  } finally {
    clearTimeout(timeout);
  }
}

/** Bound response bytes before UTF-8 and JSON decoding. */
export async function readAciJson(response: Response, signal?: AbortSignal): Promise<unknown> {
  const tooLarge = () => new AciError('ACI artifact response exceeds maximum byte size (32 MiB)');
  if (Number(response.headers.get('content-length')) > MAX_ATTESTATION_RESPONSE_BYTES) {
    await response.body?.cancel();
    throw tooLarge();
  }
  const reader = response.body?.getReader();
  if (!reader) throw new AciError('ACI artifact response has no body');
  let bytes = new Uint8Array(8192);
  let size = 0;
  // Custom fetch implementations may not propagate request aborts to the body.
  const abort = () => { void reader.cancel(signal?.reason).catch(() => {}); };
  signal?.addEventListener('abort', abort, { once: true });
  if (signal?.aborted) abort();
  try {
    signal?.throwIfAborted();
    while (true) {
      const { done, value } = await reader.read();
      signal?.throwIfAborted();
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
    signal?.removeEventListener('abort', abort);
    reader.releaseLock();
  }
  return JSON.parse(new TextDecoder().decode(bytes.subarray(0, size)));
}
