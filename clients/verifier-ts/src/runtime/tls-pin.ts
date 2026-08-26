import { X509Certificate, createHash, timingSafeEqual } from 'node:crypto';
import { checkServerIdentity as checkTlsServerIdentity } from 'node:tls';

import { AciConnectionError } from './types.js';

export function createPinnedServerIdentityCheck(
  expectedHostname: string,
  spkiPins: readonly string[],
): NonNullable<import('node:tls').ConnectionOptions['checkServerIdentity']> {
  const expectedPins = spkiPins.map(normalizePin);
  if (expectedPins.length === 0) {
    throw new AciConnectionError('invalid_tls_pin', 'at least one attested TLS SPKI is required');
  }

  return (hostname, cert) => {
    const hostnameError = checkTlsServerIdentity(expectedHostname, cert);
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
  };
}

export function requestUrl(input: RequestInfo | URL): URL {
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
