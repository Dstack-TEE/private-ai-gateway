import { Hono } from 'hono';
import { bearerAuth } from 'hono/bearer-auth';

import { loadConfig } from './config';

/**
 * Control plane — config-driven, no database. It implements the same
 * executor<->control HTTP surface as the production control, so a private
 * control image is a drop-in replacement.
 *
 * Content-blind, like any control plane: it only ever sees `{ apiKeyHash, model }`
 * and post-request usage counts — never plaintext, never provider credentials.
 */
const config = loadConfig();
const allowList = new Set(config.keys ?? []);
const requireKey = allowList.size > 0;

export const app = new Hono();

// When exposed over the network, authenticate consult/catalog with a bearer
// token (set PRIVATE_AI_GATEWAY_CONTROL_TOKEN). Unset = local dev (no auth).
const CONTROL_TOKEN = process.env.PRIVATE_AI_GATEWAY_CONTROL_TOKEN?.trim();
if (CONTROL_TOKEN) {
  app.use('/consult/*', bearerAuth({ token: CONTROL_TOKEN }));
  app.use('/models', bearerAuth({ token: CONTROL_TOKEN }));
}

// Liveness/identity probe.
app.get('/', (c) => c.text('private-ai-gateway control plane\n'));

// Model catalog. The executor's /v1/models proxies here.
app.get('/models', (c) =>
  c.json({ data: Object.keys(config.models).map((id) => ({ id, object: 'model' })) })
);

// Pre-request consult (content-blind): authorize + resolve pricing + ordered
// candidates from config. A denial carries the status + message the executor
// returns verbatim.
app.post('/consult/pre', async (c) => {
  const body = (await c.req.json().catch(() => ({}))) as {
    apiKeyHash?: string;
    model?: string;
  };
  const model = body.model ?? '';
  if (!model) {
    return c.json({ allow: false, status: 400, message: 'Model parameter is required' });
  }
  if (requireKey && (!body.apiKeyHash || !allowList.has(body.apiKeyHash))) {
    return c.json({ allow: false, status: 401, message: 'Invalid API key' });
  }
  const entry = config.models[model];
  if (!entry) {
    return c.json({ allow: false, status: 404, message: `Unknown model: ${model}` });
  }
  return c.json({ allow: true, pricing: entry.pricing ?? null, candidates: entry.candidates });
});

// Post-request consult (content-blind): this build does no billing — it accepts
// the usage report and drops it. (The production control records it.)
app.post('/consult/post', async (c) => {
  await c.req.json().catch(() => undefined);
  return c.json({ ok: true });
});
