export interface PinnedTransportOptions {
  origin: string;
  hostname: string;
  spkiPins: readonly string[];
  proxy?: string;
  ca?: string | Buffer;
}

export interface PinnedTransport {
  fetch: AciFetch;
  close(): Promise<void>;
}

export type PinnedTransportFactory = (options: PinnedTransportOptions) => PinnedTransport;
import type { AciFetch } from './types.js';
