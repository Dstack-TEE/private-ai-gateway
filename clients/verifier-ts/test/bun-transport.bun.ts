import { after, test } from 'node:test';

import { createPinnedTransport } from '../src/bun/transport.js';
import {
  assertPinnedTransport,
  cleanupPinnedTransportFixture,
} from './pinned-transport-fixture.js';

after(cleanupPinnedTransportFixture);

test('Bun fetch enforces the shared pinned transport contract', () =>
  assertPinnedTransport(createPinnedTransport));
