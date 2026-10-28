import type { AttestationReport } from '../types.js';
import { AciConnectionError, type ConnectAciOptions } from './types.js';

const MULTIPART_PROVIDER_HEADER = 'x-aci-provider';
const MAX_MULTIPART_PROVIDER_BYTES = 8192;
const AUDIO_TRANSCRIPTIONS_PATH = '/v1/audio/transcriptions';

interface EffectivePolicy {
  provider: Record<string, unknown>;
  pinnedSessions: string[];
  requiresVerified: boolean;
}

interface ConstrainedBody {
  body: Uint8Array;
  pinnedSessions: string[];
  requiresVerified: boolean;
}

function invalid(message: string): never {
  throw new AciConnectionError('invalid_serving_constraints', message);
}

/** Check raw sources before Fetch combines case-insensitive duplicate names. */
export function validateProviderHeaderMultiplicity(headers: HeadersInit | undefined): void {
  if (headers === undefined || headers instanceof Headers) return;
  const names = Array.isArray(headers) ? headers.map(([name]) => name) : Object.keys(headers);
  if (names.filter((name) => name.toLowerCase() === MULTIPART_PROVIDER_HEADER).length > 1) {
    invalid('duplicate X-ACI-Provider headers');
  }
}

export function object(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

function isConstrained(options: ConnectAciOptions): boolean {
  return options.serving?.requireVerified !== false
    || (options.serving?.acceptedSessionIds?.length ?? 0) > 0;
}

function mergeProvider(value: unknown, options: ConnectAciOptions): EffectivePolicy {
  if (!object(value)) invalid('provider must be a JSON object');
  const provider = { ...value };
  for (const key of Object.keys(provider)) {
    if (key.startsWith('aci_') && key !== 'aci_verified' && key !== 'aci_session_ids') {
      invalid('unknown aci_ serving constraint');
    }
  }
  if (provider.aci_verified !== undefined && typeof provider.aci_verified !== 'boolean') {
    invalid('provider.aci_verified must be a boolean');
  }
  const supplied = parseSessionIds(provider.aci_session_ids);
  if (supplied.length > 0 && provider.aci_verified === false) {
    invalid('provider.aci_session_ids cannot be combined with aci_verified=false');
  }
  const fixed = [...(options.serving?.acceptedSessionIds ?? [])];
  const pinnedSessions = fixed.length === 0
    ? supplied
    : supplied.length === 0
      ? fixed
      : supplied.filter((id) => fixed.includes(id));
  if (fixed.length > 0 && supplied.length > 0 && pinnedSessions.length === 0) {
    invalid('request session ids do not intersect the connection acceptedSessionIds policy');
  }
  const requiresVerified = isConstrained(options)
    || provider.aci_verified === true || pinnedSessions.length > 0;
  if (pinnedSessions.length > 0) provider.aci_session_ids = pinnedSessions;
  if (requiresVerified) provider.aci_verified = true;
  return { provider, pinnedSessions, requiresVerified };
}

export function constrainJsonBody(body: Uint8Array, options: ConnectAciOptions): ConstrainedBody {
  const constrained = isConstrained(options);
  const unchanged = { body, pinnedSessions: [], requiresVerified: false };
  let root: unknown;
  try {
    root = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(body));
  } catch {
    if (constrained) invalid('JSON request body must be valid JSON');
    return unchanged;
  }
  if (!object(root)) {
    if (constrained) invalid('JSON request body must have an object top level');
    return unchanged;
  }
  if (root.provider !== undefined && root.provider !== null && !object(root.provider)) {
    if (constrained) invalid('provider must be a JSON object or null');
    return unchanged;
  }
  const effective = mergeProvider(root.provider ?? {}, options);
  if (!effective.requiresVerified) return unchanged;
  root.provider = effective.provider;
  const outgoing = new TextEncoder().encode(JSON.stringify(root));
  return {
    body: outgoing,
    pinnedSessions: effective.pinnedSessions,
    requiresVerified: effective.requiresVerified,
  };
}

export async function constrainRequestBody(
  body: Uint8Array,
  headers: Headers,
  path: string,
  options: ConnectAciOptions,
  capabilities: AttestationReport['service_capabilities'],
): Promise<ConstrainedBody> {
  const connectionTokens = headers.get('connection')?.toLowerCase().split(',').map((name) => name.trim()) ?? [];
  if (connectionTokens.includes(MULTIPART_PROVIDER_HEADER) || connectionTokens.includes('content-type')) {
    invalid('serving constraint carriers cannot be hop-by-hop headers');
  }
  const contentType = headers.get('content-type') ?? '';
  const mediaType = contentType.split(';')[0]?.trim().toLowerCase() ?? '';
  const multipart = mediaType === 'multipart/form-data';
  const json = mediaType === 'application/json'
    || (mediaType.startsWith('application/') && mediaType.endsWith('+json'));
  const carrier = headers.get(MULTIPART_PROVIDER_HEADER);
  if (carrier !== null && (!multipart || path !== AUDIO_TRANSCRIPTIONS_PATH)) {
    invalid('X-ACI-Provider is only valid on supported multipart endpoints');
  }
  const constrained = isConstrained(options);
  if (multipart && path === AUDIO_TRANSCRIPTIONS_PATH) {
    let provider: unknown = {};
    if (carrier !== null) {
      if (/[\u0080-\uffff]/.test(carrier)) {
        invalid('X-ACI-Provider must contain only US-ASCII bytes');
      }
      if (Buffer.byteLength(carrier) > MAX_MULTIPART_PROVIDER_BYTES) {
        invalid('X-ACI-Provider exceeds 8192 bytes');
      }
      try {
        // Headers/Request inputs have already lost header multiplicity.
        // JSON.parse still rejects coalesced complete objects and malformed carriers.
        provider = JSON.parse(carrier);
      } catch {
        invalid('X-ACI-Provider must contain a single JSON object');
      }
    }
    const effective = mergeProvider(provider, options);
    if ((constrained || carrier !== null)
      && !capabilities?.supported_multipart_constraint_endpoints?.includes(path)) {
      invalid('gateway does not advertise multipart serving constraints for this endpoint');
    }
    await rejectMultipartProvider(body, contentType);
    if (constrained || carrier !== null) {
      const value = JSON.stringify(effective.provider).replace(/[\u0080-\uffff]/g,
        (unit) => `\\u${unit.charCodeAt(0).toString(16).padStart(4, '0')}`);
      if (Buffer.byteLength(value) > MAX_MULTIPART_PROVIDER_BYTES) {
        invalid('effective X-ACI-Provider exceeds 8192 bytes');
      }
      headers.set(MULTIPART_PROVIDER_HEADER, value);
    }
    return {
      body,
      pinnedSessions: effective.pinnedSessions,
      requiresVerified: effective.requiresVerified,
    };
  }
  if (json) return constrainJsonBody(body, options);
  if (constrained && body.length > 0) {
    invalid('cannot apply serving constraints to this request media type');
  }
  return { body, pinnedSessions: [], requiresVerified: constrained };
}

/** Validate multipart with the platform parser while retaining the wire bytes. */
async function rejectMultipartProvider(body: Uint8Array, contentType: string): Promise<void> {
  try {
    const form = await new Response(Buffer.from(body), {
      headers: { 'content-type': contentType },
    }).formData();
    if (!form.has('model')) invalid('multipart body must contain a model part');
    if (form.has('provider')) {
      invalid('multipart provider form parts are forbidden; use X-ACI-Provider');
    }
  } catch (error) {
    if (error instanceof AciConnectionError) throw error;
    invalid('malformed multipart body or boundary');
  }
}

function parseSessionIds(value: unknown): string[] {
  if (value === undefined) return [];
  if (!Array.isArray(value) || value.length === 0
    || value.some((id) => typeof id !== 'string' || !/^[0-9a-f]{64}$/.test(id))) {
    invalid('provider.aci_session_ids must be a non-empty array of lowercase 64-hex ids');
  }
  return [...new Set(value)];
}
