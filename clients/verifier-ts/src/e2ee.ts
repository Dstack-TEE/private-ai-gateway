/**
 * E2EE v2.1 client (spec/e2ee-v2.md §10), X25519 suite, Web Crypto only.
 *
 * {@link createE2eeFetch} wraps `fetch`: it encrypts each supported request
 * just before it is sent and decrypts the response, buffered or streamed. Pass
 * it as the `fetch` option of an OpenAI-compatible SDK so it sees the exact
 * body that goes on the wire.
 */

import { fromHex, sha256, toHex } from './crypto.js';
import { AciError } from './errors.js';
import type { ReportTranscript } from './transcript.js';

/** The v2.1 X25519 keyset `algo` (§10.1). */
export const E2EE_V2_1_X25519 = 'x25519-aes-256-gcm-hkdf-sha256-v2.1';

const VERSION = '2.1';
const FIELD_INFO = new TextEncoder().encode('aci.e2ee.v2.1.x25519');
const REQUEST_KEY_INFO = new TextEncoder().encode('aci.e2ee.v2.1.x25519.request-key');
const MAC_INFO = new TextEncoder().encode('aci.e2ee.v2.1.response-mac');
const MAC_MEMBER = 'e2ee_mac';
const NO_SALT = new Uint8Array(0);
const KEY_LEN = 32;
const NONCE_LEN = 12;
const SECRET_LEN = 32;
const E2EE_REQUEST_HEADERS = [
  'x-signing-algo',
  'x-client-pub-key',
  'x-model-pub-key',
  'x-e2ee-version',
  'x-e2ee-nonce',
  'x-e2ee-timestamp',
  'x-e2ee-request-key',
];

const subtle = globalThis.crypto.subtle;

/** An E2EE v2.1 failure. The request was not sent, or its response was refused. */
export class E2eeError extends AciError {
  constructor(message: string) {
    super(message);
    this.name = 'E2eeError';
  }
}

/**
 * The verified service changed or expired: its keyset passed `not_after`, or a
 * response carried a different `X-ACI-Keyset-Digest`. Run `verifyService`
 * again and create a new client from the new transcript.
 */
export class E2eeReverifyError extends E2eeError {
  constructor(message: string) {
    super(`${message}; verify the service again`);
    this.name = 'E2eeReverifyError';
  }
}

/** The `fetch` call shape this client takes and returns. */
export type E2eeFetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export interface E2eeFetchOptions {
  /**
   * A VERIFIED transcript whose id-6 passed with `channel: { e2ee:
   * E2EE_V2_1_X25519 }`, e.g. from `verifyService(url, { acceptedComposeHashes,
   * channel })`.
   */
  transcript: ReportTranscript;
  /** The underlying `fetch`. Defaults to `globalThis.fetch`. */
  fetch?: E2eeFetch;
}

type Endpoint = 'chat' | 'completions' | 'embeddings';

/** Per-request state, kept until the response is fully decrypted. */
export interface RequestContext {
  model: string;
  nonce: string;
  ts: number;
  secret: Uint8Array;
  clientPrivateKey: CryptoKey;
  clientPublicKeyHex: string;
  macKey: CryptoKey;
  /** The request asked for an SSE stream; the response must be one. */
  stream: boolean;
}

/**
 * A `fetch` that sends POST requests to chat completions, completions, and
 * embeddings under E2EE v2.1 and decrypts their responses. Other POST
 * requests are refused rather than sent in plaintext; other methods pass
 * through unchanged.
 *
 * Throws {@link E2eeError} at creation unless the transcript is VERIFIED with
 * the E2EE v2.1 channel binding and its keyset offers a v2.1 X25519 key.
 * There is no fallback to v2, the legacy scheme, or plaintext (§10.1). Sends
 * throw {@link E2eeReverifyError} once the keyset expires or the service
 * reports a different keyset digest (ACI §5.2).
 */
export function createE2eeFetch(options: E2eeFetchOptions): E2eeFetch {
  const { transcript } = options;
  const channel = transcript.channel;
  const bound =
    channel !== undefined &&
    'e2ee' in channel &&
    channel.e2ee === E2EE_V2_1_X25519 &&
    transcript.lines.some((entry) => entry.id === 'id-6' && entry.status === 'pass');
  if (!transcript.verdict.verified || !bound) {
    throw new E2eeError(
      `the transcript must be VERIFIED with channel { e2ee: ${E2EE_V2_1_X25519} }: ${transcript.verdict.line}`,
    );
  }
  const { keyset, workloadKeysetDigest } = transcript.verification;
  const serviceKey = keyset?.e2ee_public_keys.find((key) => key.algo === E2EE_V2_1_X25519);
  if (keyset === undefined || workloadKeysetDigest === undefined || serviceKey === undefined) {
    throw new E2eeError(`the verified keyset lists no ${E2EE_V2_1_X25519} key`);
  }
  const notAfter = keyset.not_after;
  const servicePublicKeyHex = serviceKey.public_key.replace(/^0x/i, '').toLowerCase();
  const servicePublicKey = importX25519Public(fromHex(servicePublicKeyHex));
  const baseFetch: E2eeFetch = options.fetch ?? ((input, init) => globalThis.fetch(input, init));
  // Set once a response names another keyset; every later send is refused.
  let changedDigest: string | undefined;

  return async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const request = new Request(input, init);
    if (request.method !== 'POST') return baseFetch(request);
    const endpoint = endpointOf(new URL(request.url).pathname);
    if (endpoint === undefined) {
      throw new E2eeError(`E2EE v2.1 does not cover POST ${new URL(request.url).pathname}`);
    }
    if (changedDigest !== undefined) {
      throw new E2eeReverifyError(`the service now serves keyset ${changedDigest}, not ${workloadKeysetDigest}`);
    }
    if (Date.now() / 1000 >= notAfter) {
      throw new E2eeReverifyError('the verified keyset has expired (not_after)');
    }

    const payload = parseJsonObject(await request.text(), 'request body');
    for (const key of UNENCRYPTED_REQUEST_MEMBERS) {
      if (key in payload) throw new E2eeError(`E2EE v2.1 does not encrypt "${key}"; remove it`);
    }
    const ctx = await newContext(payload);
    await sealRequestFields(payload, endpoint, ctx, await servicePublicKey);
    const digest = await sha256(canonicalJson(payload));
    const requestKey = await seal(
      await servicePublicKey,
      concat(ctx.secret, digest),
      canonicalJson({
        purpose: 'aci.e2ee.request-key.v2.1',
        algo: E2EE_V2_1_X25519,
        model: ctx.model,
        client_pub_key: ctx.clientPublicKeyHex,
        nonce: ctx.nonce,
        ts: ctx.ts,
      }),
      NO_SALT,
      REQUEST_KEY_INFO,
    );

    const headers = new Headers(request.headers);
    for (const name of E2EE_REQUEST_HEADERS) headers.delete(name);
    headers.delete('content-length');
    headers.set('content-type', 'application/json');
    headers.set('x-e2ee-version', VERSION);
    headers.set('x-client-pub-key', ctx.clientPublicKeyHex);
    headers.set('x-model-pub-key', servicePublicKeyHex);
    headers.set('x-e2ee-nonce', ctx.nonce);
    headers.set('x-e2ee-timestamp', String(ctx.ts));
    headers.set('x-e2ee-request-key', requestKey);

    const response = await baseFetch(request.url, {
      method: 'POST',
      headers,
      body: JSON.stringify(payload),
      signal: request.signal,
    });
    // Headers are hints, but a changed one gates new sends (ACI §5.2). This
    // response, made for the verified key, still opens.
    const servedDigest = response.headers.get('x-aci-keyset-digest');
    if (servedDigest !== null && servedDigest.trim().toLowerCase() !== workloadKeysetDigest.toLowerCase()) {
      changedDigest = servedDigest;
    }
    return openResponse(response, endpoint, ctx);
  };
}

async function newContext(payload: Record<string, unknown>): Promise<RequestContext> {
  if (typeof payload.model !== 'string') {
    throw new E2eeError('request body has no string "model"');
  }
  // A fresh reply key per request: nothing links one request to the next.
  const pair = (await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits'])) as CryptoKeyPair;
  const secret = randomBytes(SECRET_LEN);
  const macIkm = await subtle.importKey('raw', secret as BufferSource, 'HKDF', false, ['deriveKey']);
  return {
    model: payload.model,
    nonce: toHex(randomBytes(32)),
    ts: Math.floor(Date.now() / 1000),
    secret,
    clientPrivateKey: pair.privateKey,
    clientPublicKeyHex: toHex(new Uint8Array(await subtle.exportKey('raw', pair.publicKey))),
    macKey: await subtle.deriveKey(
      { name: 'HKDF', hash: 'SHA-256', salt: NO_SALT as BufferSource, info: MAC_INFO as BufferSource },
      macIkm,
      { name: 'HMAC', hash: 'SHA-256', length: 256 },
      false,
      ['verify'],
    ),
    stream: payload.stream === true,
  };
}

/** Request members v2.1 leaves readable that can carry user content (§10.5). */
const UNENCRYPTED_REQUEST_MEMBERS = ['prediction', 'functions', 'function_call'];

function endpointOf(pathname: string): Endpoint | undefined {
  if (pathname.endsWith('/chat/completions')) return 'chat';
  if (pathname.endsWith('/completions')) return 'completions';
  if (pathname.endsWith('/embeddings')) return 'embeddings';
  return undefined;
}

// ---- Request fields (§5, §10.5) ---------------------------------------------

async function sealRequestFields(
  payload: Record<string, unknown>,
  endpoint: Endpoint,
  ctx: RequestContext,
  serviceKey: CryptoKey,
): Promise<void> {
  const sealField = (field: string, plaintext: string): Promise<string> =>
    seal(
      serviceKey,
      new TextEncoder().encode(plaintext),
      fieldAad('aci.e2ee.request.v2.1', ctx, field),
      ctx.secret,
      FIELD_INFO,
    );

  if (endpoint === 'chat') {
    const messages = payload.messages;
    if (!Array.isArray(messages)) throw new E2eeError('chat request has no "messages" array');
    for (const [m, message] of messages.entries()) {
      if (!isObject(message)) continue;
      if ('function_call' in message) {
        throw new E2eeError('E2EE v2.1 does not encrypt "function_call"; use tool_calls');
      }
      // Whole content is the JSON encoding of the content value (§10.5).
      if (typeof message.content === 'string' || Array.isArray(message.content)) {
        message.content = await sealField(`messages.${m}.content`, JSON.stringify(message.content));
      }
      // Reasoning a client echoes back from an earlier reply (§10.5).
      for (const key of ['reasoning', 'reasoning_content']) {
        const value = message[key];
        if (typeof value === 'string') message[key] = await sealField(`messages.${m}.${key}`, value);
      }
      for (const [t, call] of toolCalls(message)) {
        const fn = call.function;
        if (isObject(fn) && typeof fn.arguments === 'string') {
          fn.arguments = await sealField(
            `messages.${m}.tool_calls.${t}.function.arguments`,
            fn.arguments,
          );
        }
      }
    }
    return;
  }

  const key = endpoint === 'completions' ? 'prompt' : 'input';
  const value = payload[key];
  if (typeof value === 'string') {
    payload[key] = await sealField(key, value);
  } else if (Array.isArray(value)) {
    for (const [i, item] of value.entries()) {
      // Token-id arrays pass through unencrypted (§5).
      if (typeof item === 'string') value[i] = await sealField(`${key}.${i}`, item);
    }
  }
}

// ---- Responses (§5, §10.5) --------------------------------------------------

/** Check and decrypt one response (§10.6). Not part of the package API. */
export async function openResponse(
  response: Response,
  endpoint: Endpoint,
  ctx: RequestContext,
): Promise<Response> {
  // Errors are unauthenticated hints (§9): hand them back as they are, never retry
  // in another mode.
  if (!response.ok) return response;
  const applied = response.headers.get('x-e2ee-applied');
  const version = response.headers.get('x-e2ee-version');
  if ((applied !== null && applied !== 'true') || (version !== null && version !== VERSION)) {
    throw new E2eeError(`response is not E2EE v2.1 (applied=${applied}, version=${version})`);
  }
  // The request decides the response kind (§10.6).
  const isSse = (response.headers.get('content-type') ?? '').toLowerCase().startsWith('text/event-stream');
  if (isSse !== ctx.stream) {
    throw new E2eeError(ctx.stream ? 'expected an SSE stream' : 'expected a JSON body, got an SSE stream');
  }

  const headers = new Headers(response.headers);
  headers.delete('content-length');
  headers.delete('content-encoding');
  const init = { status: response.status, statusText: response.statusText, headers };
  if (ctx.stream) {
    if (response.body === null) throw new E2eeError('streamed response has no body');
    return new Response(response.body.pipeThrough(openSseStream(endpoint, ctx)), init);
  }
  const payload = await openObject(await response.text(), endpoint, ctx);
  return new Response(JSON.stringify(payload), init);
}

/** Check the MAC of one response object, then decrypt its fields (§10.6). */
async function openObject(text: string, endpoint: Endpoint, ctx: RequestContext): Promise<Record<string, unknown>> {
  const payload = parseJsonObject(text, 'response object');
  const mac = payload[MAC_MEMBER];
  if (typeof mac !== 'string' || !/^[0-9a-f]{64}$/.test(mac)) {
    throw new E2eeError(`response object has no ${MAC_MEMBER}`);
  }
  delete payload[MAC_MEMBER];
  const authentic = await subtle.verify(
    'HMAC',
    ctx.macKey,
    fromHex(mac) as BufferSource,
    canonicalJson(payload) as BufferSource,
  );
  if (!authentic) throw new E2eeError('response object failed its E2EE v2.1 MAC');
  await openResponseFields(payload, endpoint, ctx);
  return payload;
}

async function openResponseFields(
  payload: Record<string, unknown>,
  endpoint: Endpoint,
  ctx: RequestContext,
): Promise<void> {
  const id = typeof payload.id === 'string' ? payload.id : '';
  const open = async (container: Record<string, unknown>, key: string, field: string) => {
    const value = container[key];
    if (value === undefined || value === null) return;
    if (typeof value !== 'string') throw new E2eeError(`${field} is not a ciphertext string`);
    const plaintext = await openBytes(
      ctx.clientPrivateKey,
      value,
      fieldAad('aci.e2ee.response.v2.1', ctx, field, id),
      ctx.secret,
      FIELD_INFO,
    );
    container[key] = new TextDecoder('utf-8', { fatal: true }).decode(plaintext);
  };
  const openJson = async (container: Record<string, unknown>, key: string, field: string) => {
    await open(container, key, field);
    if (typeof container[key] === 'string') container[key] = JSON.parse(container[key] as string);
  };

  if (endpoint === 'embeddings') {
    for (const [i, entry] of indexed(payload.data, 'data')) {
      await openJson(entry, 'embedding', `data.${i}.embedding`);
    }
    return;
  }
  for (const [i, choice] of indexed(payload.choices, 'choices')) {
    await openJson(choice, 'logprobs', `choices.${i}.logprobs`);
    if (endpoint === 'completions') {
      await open(choice, 'text', `choices.${i}.text`);
      continue;
    }
    const part = ctx.stream ? 'delta' : 'message';
    const message = choice[part];
    if (!isObject(message)) continue;
    const prefix = `choices.${i}.${part}`;
    for (const key of ['content', 'reasoning', 'reasoning_content', 'refusal']) {
      await open(message, key, `${prefix}.${key}`);
    }
    if (!ctx.stream && isObject(message.audio)) {
      await open(message.audio, 'data', `${prefix}.audio.data`);
    }
    for (const [t, call] of toolCalls(message)) {
      if (isObject(call.function)) {
        await open(call.function, 'arguments', `${prefix}.tool_calls.${t}.function.arguments`);
      }
    }
  }
}

/**
 * Check and decrypt each `data:` event of an SSE stream (§10.6). `[DONE]`
 * passes through, comments are dropped, and any other line fails the stream.
 */
function openSseStream(endpoint: Endpoint, ctx: RequestContext): TransformStream<Uint8Array, Uint8Array> {
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  let buffered = '';
  // Every event of one stream carries the same response id (§10.6).
  let streamId: unknown;
  const flushEvents = async (
    controller: TransformStreamDefaultController<Uint8Array>,
    final: boolean,
  ): Promise<void> => {
    const events = buffered.split(/\r?\n\r?\n/);
    buffered = final ? '' : (events.pop() ?? '');
    for (const event of events) {
      const data: string[] = [];
      for (const line of event.split(/\r?\n/)) {
        if (line === '' || line.startsWith(':')) continue;
        if (!line.startsWith('data:')) throw new E2eeError('unexpected line in E2EE v2.1 stream');
        data.push(line.slice('data:'.length).replace(/^ /, ''));
      }
      if (data.length === 0) continue;
      const joined = data.join('\n');
      let rendered = joined;
      if (joined !== '[DONE]') {
        const payload = await openObject(joined, endpoint, ctx);
        streamId ??= payload.id;
        if (payload.id !== streamId) throw new E2eeError('stream event belongs to another response');
        rendered = JSON.stringify(payload);
      }
      controller.enqueue(encoder.encode(`data: ${rendered}\n\n`));
    }
  };
  return new TransformStream({
    async transform(chunk, controller) {
      buffered += decoder.decode(chunk, { stream: true });
      await flushEvents(controller, false);
    },
    async flush(controller) {
      buffered += decoder.decode();
      await flushEvents(controller, true);
    },
  });
}

// ---- Field crypto (§4, §10.4) -----------------------------------------------

function fieldAad(purpose: string, ctx: RequestContext, field: string, id?: string): Uint8Array {
  return canonicalJson({
    purpose,
    algo: E2EE_V2_1_X25519,
    model: ctx.model,
    ...(id === undefined ? {} : { id }),
    field,
    nonce: ctx.nonce,
    ts: ctx.ts,
  });
}

/**
 * Encrypt to an X25519 public key: `ephemeral_public || nonce || ciphertext || tag`,
 * hex. `ephemeral` and `nonce` are fixed only by tests. Not part of the package API.
 */
export async function seal(
  recipient: CryptoKey,
  plaintext: Uint8Array,
  aad: Uint8Array,
  salt: Uint8Array,
  info: Uint8Array,
  fixed?: { ephemeral: CryptoKeyPair; nonce: Uint8Array },
): Promise<string> {
  const ephemeral =
    fixed?.ephemeral ??
    ((await subtle.generateKey({ name: 'X25519' }, true, ['deriveBits'])) as CryptoKeyPair);
  const nonce = fixed?.nonce ?? randomBytes(NONCE_LEN);
  const key = await fieldKey(ephemeral.privateKey, recipient, salt, info);
  const ciphertext = new Uint8Array(
    await subtle.encrypt(
      { name: 'AES-GCM', iv: nonce as BufferSource, additionalData: aad as BufferSource },
      key,
      plaintext as BufferSource,
    ),
  );
  const ephemeralPublic = new Uint8Array(await subtle.exportKey('raw', ephemeral.publicKey));
  return toHex(concat(ephemeralPublic, nonce, ciphertext));
}

/** Decrypt a {@link seal} ciphertext with the recipient's private key. Not part of the package API. */
export async function openBytes(
  recipient: CryptoKey,
  ciphertextHex: string,
  aad: Uint8Array,
  salt: Uint8Array,
  info: Uint8Array,
): Promise<Uint8Array> {
  let blob: Uint8Array;
  try {
    blob = fromHex(ciphertextHex);
  } catch {
    throw new E2eeError('ciphertext is not hex');
  }
  if (blob.length < KEY_LEN + NONCE_LEN + 16) throw new E2eeError('ciphertext is too short');
  const ephemeral = await importX25519Public(blob.subarray(0, KEY_LEN));
  const key = await fieldKey(recipient, ephemeral, salt, info);
  try {
    return new Uint8Array(
      await subtle.decrypt(
        {
          name: 'AES-GCM',
          iv: blob.subarray(KEY_LEN, KEY_LEN + NONCE_LEN) as BufferSource,
          additionalData: aad as BufferSource,
        },
        key,
        blob.subarray(KEY_LEN + NONCE_LEN) as BufferSource,
      ),
    );
  } catch {
    throw new E2eeError('field does not decrypt under E2EE v2.1');
  }
}

async function fieldKey(
  privateKey: CryptoKey,
  publicKey: CryptoKey,
  salt: Uint8Array,
  info: Uint8Array,
): Promise<CryptoKey> {
  const shared = await subtle.deriveBits({ name: 'X25519', public: publicKey }, privateKey, 256);
  const ikm = await subtle.importKey('raw', shared, 'HKDF', false, ['deriveKey']);
  // An empty HKDF salt is the same as none (RFC 5869 §2.2).
  return subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: salt as BufferSource, info: info as BufferSource },
    ikm,
    { name: 'AES-GCM', length: 256 },
    false,
    ['encrypt', 'decrypt'],
  );
}

function importX25519Public(raw: Uint8Array): Promise<CryptoKey> {
  return subtle.importKey('raw', raw as BufferSource, { name: 'X25519' }, true, []);
}

// ---- JSON -------------------------------------------------------------------

/**
 * RFC 8785 JCS bytes of a JSON value. Member names sort by UTF-16 code units
 * and numbers use the ECMAScript form, which is what JSON.stringify emits.
 * Objects are written directly rather than rebuilt, because JS objects list
 * integer-like keys (e.g. `logit_bias` token ids) before all others.
 */
export function canonicalJson(value: unknown): Uint8Array {
  return new TextEncoder().encode(canonicalString(value));
}

function canonicalString(value: unknown): string {
  if (value === null || typeof value === 'boolean' || typeof value === 'string') {
    return JSON.stringify(value);
  }
  if (typeof value === 'number') {
    if (!Number.isFinite(value)) throw new E2eeError('JSON numbers must be finite');
    return JSON.stringify(value);
  }
  if (Array.isArray(value)) return `[${value.map(canonicalString).join(',')}]`;
  if (isObject(value)) {
    const members = Object.keys(value)
      .filter((key) => value[key] !== undefined)
      .sort()
      .map((key) => `${JSON.stringify(key)}:${canonicalString(value[key])}`);
    return `{${members.join(',')}}`;
  }
  throw new E2eeError(`value of type ${typeof value} is not JSON`);
}

function parseJsonObject(text: string, what: string): Record<string, unknown> {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    throw new E2eeError(`${what} is not JSON`);
  }
  if (!isObject(value)) throw new E2eeError(`${what} is not a JSON object`);
  return value;
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value);
}

/**
 * Entries of an array with their path index: the `index` member when it is a
 * non-negative integer, else the position (§5). A present non-array is refused.
 */
function indexed(value: unknown, name: string): Array<[number, Record<string, unknown>]> {
  if (value === undefined || value === null) return [];
  if (!Array.isArray(value)) throw new E2eeError(`"${name}" is not an array`);
  return value.flatMap((entry, position): Array<[number, Record<string, unknown>]> => {
    if (!isObject(entry)) return [];
    const index = entry.index;
    return [[typeof index === 'number' && Number.isInteger(index) && index >= 0 ? index : position, entry]];
  });
}

function toolCalls(message: Record<string, unknown>): Array<[number, Record<string, unknown>]> {
  return indexed(message.tool_calls, 'tool_calls');
}

function randomBytes(length: number): Uint8Array {
  return globalThis.crypto.getRandomValues(new Uint8Array(length));
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const out = new Uint8Array(parts.reduce((n, part) => n + part.length, 0));
  let offset = 0;
  for (const part of parts) {
    out.set(part, offset);
    offset += part.length;
  }
  return out;
}
