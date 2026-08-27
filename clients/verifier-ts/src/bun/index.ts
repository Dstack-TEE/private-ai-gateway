import { connectAciWithTransport } from '../runtime/connection.js';
import type { ConnectAciOptions } from '../runtime/types.js';
import { createPinnedTransport } from './transport.js';

export function connectAci(options: ConnectAciOptions) {
  return connectAciWithTransport(options, createPinnedTransport);
}

export * from '../runtime/types.js';
