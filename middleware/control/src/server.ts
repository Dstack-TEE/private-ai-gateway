#!/usr/bin/env node
import { existsSync, unlinkSync } from 'node:fs';

import { createAdaptorServer } from '@hono/node-server';

import { app } from './app';

/**
 * Listen on the control Unix domain socket the executor dials, with a TCP
 * fallback for local development. No background workers / no database.
 */
const socketPath = process.env.PRIVATE_AI_GATEWAY_CONTROL_UDS_PATH?.trim();
const portArg = process.argv.slice(2).find((arg) => arg.startsWith('--port='));
const port = portArg ? Number.parseInt(portArg.split('=')[1], 10) : 8789;

const server = createAdaptorServer({ fetch: app.fetch });

if (socketPath) {
  if (existsSync(socketPath)) {
    unlinkSync(socketPath);
  }
  server.listen({ path: socketPath }, () => {
    console.log(`control listening on unix socket ${socketPath}`);
  });
} else {
  server.listen(port, () => {
    console.log(`control listening on http://localhost:${port}`);
  });
}

function shutdown(): void {
  server.close(() => {
    if (socketPath && existsSync(socketPath)) {
      try {
        unlinkSync(socketPath);
      } catch {
        // best effort
      }
    }
    process.exit(0);
  });
}

process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
