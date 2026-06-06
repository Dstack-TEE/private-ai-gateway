#!/usr/bin/env node
import { existsSync, unlinkSync } from 'node:fs';

import { createAdaptorServer } from '@hono/node-server';

import { app } from './app';

/**
 * Listen on the middleware Unix domain socket when configured (the hop
 * the frontend dials), otherwise fall back to a TCP port for local
 * single-process development.
 */
const socketPath = process.env.PRIVATE_AI_GATEWAY_MIDDLEWARE_UDS_PATH?.trim();
const portArg = process.argv.slice(2).find((arg) => arg.startsWith('--port='));
const port = portArg ? Number.parseInt(portArg.split('=')[1], 10) : 8788;

const server = createAdaptorServer({ fetch: app.fetch });

function removeStaleSocket(path: string): void {
  if (existsSync(path)) {
    unlinkSync(path);
  }
}

if (socketPath) {
  removeStaleSocket(socketPath);
  server.listen({ path: socketPath }, () => {
    console.log(`executor listening on unix socket ${socketPath}`);
  });
} else {
  server.listen(port, () => {
    console.log(`executor listening on http://localhost:${port}`);
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
