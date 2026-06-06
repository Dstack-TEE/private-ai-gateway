import { createHash } from 'node:crypto';
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

export type SpendMode = 'regular' | 'subscription' | 'subscription_overflow';

export interface PreConsult {
  allow: boolean;
  // Set when allow is false: the status + message to return to the client.
  status?: number;
  message?: string;
  pricing?: PricingConfig | null;
  // Billing identity, carried to the post-request consult.
  userId?: number;
  virtualKeyId?: number;
  spendMode?: SpendMode;
  // Set on a 429 denial: drives the X-RateLimit-* / Retry-After headers.
  rateLimit?: { limit: number; resetAt: number };
}

/** SHA-256 hex of the bearer key — only the hash crosses to the control plane. */
export function hashApiKey(apiKey: string): string {
  return createHash('sha256').update(apiKey).digest('hex');
}

/**
 * Pre-request consult: content-blind {apiKeyHash?, model} -> {allow, ...}.
 * Because this gates authorization, it fails CLOSED — an unreachable control
 * plane blocks the request (503) rather than letting it through unauthorized.
 */
export async function consultPre(
  model: string | undefined,
  apiKeyHash: string | undefined
): Promise<PreConsult> {
  try {
    const res = await controlRequest(
      'POST',
      '/consult/pre',
      JSON.stringify({ apiKeyHash, model })
    );
    if (res.status !== 200) {
      return { allow: false, status: 503, message: 'control plane unavailable' };
    }
    return JSON.parse(res.body) as PreConsult;
  } catch {
    return { allow: false, status: 503, message: 'control plane unavailable' };
  }
}

/** Fetch the model catalog (relayed by the executor's GET /v1/models). */
export function fetchCatalog(): Promise<{ status: number; body: string }> {
  return controlRequest('GET', '/models');
}
