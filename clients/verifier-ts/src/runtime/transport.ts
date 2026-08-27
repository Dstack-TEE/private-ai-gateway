export interface PinnedTransportOptions {
  origin: string;
  hostname: string;
  spkiPins: readonly string[];
  proxy?: string;
  ca?: string | Buffer;
}

export interface PinnedTransport {
  fetch: AciFetch;
  /** TLS peer SPKI observed and accepted by the latest handshake. */
  observedSpkiSha256(): string | undefined;
  close(): Promise<void>;
}

export type PinnedTransportFactory = (options: PinnedTransportOptions) => PinnedTransport;
import type { AciFetch } from './types.js';
