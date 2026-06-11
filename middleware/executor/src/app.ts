import { Hono } from 'hono';

import {
  chatCompletions,
  completions,
  embeddings,
  messages,
  models,
  responses,
} from './handlers';

/**
 * The executor's HTTP surface. The gateway frontend dials this over a
 * Unix domain socket and sends plaintext OpenAI- or Anthropic-shaped
 * requests; the executor shapes them per upstream, forwards to the
 * gateway backend, and relays the (format-converted) response.
 */
export const app = new Hono();

// Liveness/identity probe.
app.get('/', (c) => c.text('private-ai-gateway middleware executor\n'));

app.get('/v1/models', models);
app.post('/v1/chat/completions', chatCompletions);
app.post('/v1/completions', completions);
app.post('/v1/embeddings', embeddings);
app.post('/v1/messages', messages);
app.post('/v1/responses', responses);
