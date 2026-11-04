import { AciError } from './errors.js';

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
  const signal = AbortSignal.timeout(timeoutMs);
  try {
    const response = await fetchImpl(input, { ...init, signal });
    return await consume(response, signal);
  } catch (error) {
    if (signal.aborted) throw new AciError('ACI request timed out');
    throw error;
  }
}

/** Bound response bytes before UTF-8 and JSON decoding. */
export async function readAciJson(response: Response, signal?: AbortSignal): Promise<unknown> {
  const tooLarge = () => new AciError('ACI artifact response exceeds maximum byte size (32 MiB)');
  if (Number(response.headers.get('content-length')) > MAX_ATTESTATION_RESPONSE_BYTES) {
    void response.body?.cancel().catch(() => {});
    throw tooLarge();
  }
  if (!response.body) throw new AciError('ACI artifact response has no body');
  let size = 0;
  const bounded = response.body.pipeThrough(new TransformStream<Uint8Array, Uint8Array>({
    transform(chunk, controller) {
      size += chunk.byteLength;
      if (size > MAX_ATTESTATION_RESPONSE_BYTES) throw tooLarge();
      controller.enqueue(chunk);
    },
  }), signal ? { signal } : {});
  return new Response(bounded).json();
}
