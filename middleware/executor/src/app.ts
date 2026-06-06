import { Hono } from 'hono';

/**
 * The executor's HTTP surface. The gateway frontend dials this over a
 * Unix domain socket and sends plaintext OpenAI- or Anthropic-shaped
 * requests; the executor shapes them per upstream, forwards to the
 * gateway backend, and relays the response.
 *
 * Request handlers (`/v1/chat/completions`, `/v1/completions`,
 * `/v1/messages`) are mounted in later steps.
 */
export const app = new Hono();

// Liveness/identity probe.
app.get('/', (c) => c.text('private-ai-gateway middleware executor\n'));
