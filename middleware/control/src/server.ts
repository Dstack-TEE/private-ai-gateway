#!/usr/bin/env node
import { createAdaptorServer } from '@hono/node-server';

import { app } from './app';

/**
 * Listen on a TCP port; the executor reaches it over HTTP(S) at
 * PRIVATE_AI_GATEWAY_CONTROL_URL. For production, terminate TLS in front of this
 * (a reverse proxy) and set a bearer token via PRIVATE_AI_GATEWAY_CONTROL_TOKEN
 * (enforced in app.ts).
 */
const portArg = process.argv.slice(2).find((arg) => arg.startsWith('--port='));
const portFromArg = portArg ? Number.parseInt(portArg.split('=')[1], 10) : undefined;
const portFromEnv = process.env.PRIVATE_AI_GATEWAY_CONTROL_PORT
  ? Number.parseInt(process.env.PRIVATE_AI_GATEWAY_CONTROL_PORT, 10)
  : undefined;
const port = portFromArg ?? portFromEnv ?? 8789;

const server = createAdaptorServer({ fetch: app.fetch });
server.listen(port, () => {
  console.log(`control listening on http://0.0.0.0:${port}`);
});

function shutdown(): void {
  server.close(() => process.exit(0));
}

process.on('SIGTERM', shutdown);
process.on('SIGINT', shutdown);
