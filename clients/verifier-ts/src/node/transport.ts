import { X509Certificate, createHash, timingSafeEqual } from 'node:crypto';
import { checkServerIdentity as checkTlsServerIdentity } from 'node:tls';

import { Agent, ProxyAgent } from 'undici';

import { AciConnectionError } from './types.js';

export interface PinnedTransportOptions {
  origin: string;
  hostname: string;
  spkiPins: readonly string[];
  proxy?: string;
  ca?: string | Buffer;
}

export interface PinnedTransport {
  fetch: typeof globalThis.fetch;
  close(): Promise<void>;
}

type PinnedDispatcher = Agent | ProxyAgent;

export function createPinnedTransport(options: PinnedTransportOptions): PinnedTransport {
  const expectedPins = options.spkiPins.map(normalizePin);
  if (expectedPins.length === 0) {
    throw new AciConnectionError('invalid_tls_pin', 'at least one attested TLS SPKI is required');
  }
  const tls = {
    ...(options.ca === undefined ? {} : { ca: options.ca }),
    checkServerIdentity: (hostname: string, cert: Parameters<typeof checkTlsServerIdentity>[1]) => {
      const hostnameError = checkTlsServerIdentity(options.hostname, cert);
      if (hostnameError) return hostnameError;

      let actual: string;
      try {
        const x509 = new X509Certificate(cert.raw);
        const spki = x509.publicKey.export({ type: 'spki', format: 'der' });
        actual = createHash('sha256').update(spki).digest('hex');
      } catch (error) {
        return new Error(
          `could not compute TLS SPKI for ${hostname}: ${error instanceof Error ? error.message : String(error)}`,
        );
      }
      if (expectedPins.some((expected) => hexEqual(actual, expected))) return undefined;
      return new Error(
        `TLS SPKI pin mismatch for ${hostname}: peer=${actual} expected=${expectedPins.join(',')}`,
      );
    },
  };
  const dispatcher: PinnedDispatcher = options.proxy
    ? new ProxyAgent({ uri: options.proxy, allowH2: false, requestTls: tls })
    : new Agent({ allowH2: false, connect: tls });
  let closed = false;

  return {
    fetch(input, init) {
      if (closed) {
        return Promise.reject(new AciConnectionError('closed', 'ACI transport is closed'));
      }
      const target = requestUrl(input);
      if (target.origin !== options.origin) {
        return Promise.reject(
          new AciConnectionError(
            'origin_mismatch',
            `ACI transport for ${options.origin} cannot request ${target.origin}`,
          ),
        );
      }
      return globalThis
        .fetch(input, { ...init, dispatcher } as RequestInit)
        .catch((error: unknown) => {
          const cause =
            error instanceof Error && error.cause instanceof Error ? error.cause : error;
          throw new AciConnectionError(
            'channel_binding',
            `ACI pinned request failed: ${cause instanceof Error ? cause.message : String(cause)}`,
            { cause: error },
          );
        });
    },
    async close() {
      if (closed) return;
      closed = true;
      await dispatcher.close();
    },
  };
}

function requestUrl(input: RequestInfo | URL): URL {
  try {
    if (typeof input === 'string') return new URL(input);
    if (input instanceof URL) return input;
    return new URL(input.url);
  } catch {
    throw new AciConnectionError('invalid_request_url', 'ACI transport requires an absolute URL');
  }
}

function normalizePin(value: string): string {
  const normalized = value.toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(normalized)) {
    throw new AciConnectionError('invalid_tls_pin', 'attested TLS SPKI must be 32-byte hex');
  }
  return normalized;
}

function hexEqual(actual: string, expected: string): boolean {
  const left = Buffer.from(actual, 'hex');
  const right = Buffer.from(expected, 'hex');
  return left.length === right.length && timingSafeEqual(left, right);
}
