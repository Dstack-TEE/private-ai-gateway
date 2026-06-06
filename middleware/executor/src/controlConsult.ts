import http from 'node:http';

import { PricingConfig } from './services/pricing';

const DEFAULT_CONTROL_SOCKET = '/run/private-ai-gateway/control.sock';

function controlSocketPath(): string {
  return (
    process.env.PRIVATE_AI_GATEWAY_CONTROL_UDS_PATH?.trim() ||
    DEFAULT_CONTROL_SOCKET
  );
}

function controlRequest(
  method: string,
  path: string,
  body?: string
): Promise<{ status: number; body: string }> {
  const payload = body === undefined ? undefined : Buffer.from(body);
  return new Promise((resolve, reject) => {
    const req = http.request(
      {
        socketPath: controlSocketPath(),
        path,
        method,
        headers: {
          'content-type': 'application/json',
          ...(payload ? { 'content-length': payload.byteLength } : {}),
        },
      },
      (res) => {
        let b = '';
        res.on('data', (c) => (b += c));
        res.on('end', () => resolve({ status: res.statusCode ?? 502, body: b }));
      }
    );
    req.on('error', reject);
    if (payload) req.write(payload);
    req.end();
  });
}

export interface PreConsult {
  allow: boolean;
  pricing: PricingConfig | null;
}

/**
 * Pre-request consult: content-blind {model, ...} -> {allow, pricing, ...}.
 * On any control error, fail open (allow + no pricing) so a missing control
 * plane degrades to "no cost injection" rather than blocking traffic.
 */
export async function consultPre(model: string | undefined): Promise<PreConsult> {
  try {
    const res = await controlRequest(
      'POST',
      '/consult/pre',
      JSON.stringify({ model })
    );
    if (res.status !== 200) return { allow: true, pricing: null };
    return JSON.parse(res.body) as PreConsult;
  } catch {
    return { allow: true, pricing: null };
  }
}

/** Fetch the model catalog (relayed by the executor's GET /v1/models). */
export function fetchCatalog(): Promise<{ status: number; body: string }> {
  return controlRequest('GET', '/models');
}
