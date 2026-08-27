import { AciConnectionError } from '../runtime/types.js';
import { createPinnedServerIdentityCheck, requestUrl } from '../runtime/tls-pin.js';
import type {
  PinnedTransport,
  PinnedTransportOptions,
} from '../runtime/transport.js';

export function createPinnedTransport(options: PinnedTransportOptions): PinnedTransport {
  let observedSpkiSha256: string | undefined;
  const checkServerIdentity = createPinnedServerIdentityCheck(
    options.hostname,
    options.spkiPins,
    (value) => {
      observedSpkiSha256 = value;
    },
  );
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
        .fetch(input, {
          ...init,
          keepalive: false,
          ...(options.proxy === undefined ? {} : { proxy: options.proxy }),
          tls: {
            ...(options.ca === undefined ? {} : { ca: options.ca }),
            checkServerIdentity,
          },
        })
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
    observedSpkiSha256() {
      return observedSpkiSha256;
    },
    async close() {
      closed = true;
    },
  };
}
