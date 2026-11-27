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
const SEQ_MEMBER = 'e2ee_seq';
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
    // id-6 binds the key to the workload; id-4 is what pins that workload's
    // compose, so a skipped id-4 leaves the binding unpinned.
    ['id-4', 'id-6'].every((id) =>
      transcript.lines.some((entry) => entry.id === id && entry.status === 'pass'),
    );
  if (!transcript.verdict.verified || !bound) {
    throw new E2eeError(
      `the transcript must be VERIFIED, with a measured compose (id-4) and channel { e2ee: ${E2EE_V2_1_X25519} }: ${transcript.verdict.line}`,
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

function endpointOf(pathname: string): Endpoint | undefined {
  if (pathname.endsWith('/chat/completions')) return 'chat';
  if (pathname.endsWith('/completions')) return 'completions';
  if (pathname.endsWith('/embeddings')) return 'embeddings';
  return undefined;
}

// ---- Locations (§5, §10.5) --------------------------------------------------

/**
 * Maps one present location value, given its field path, to its replacement.
 * Every v2.1 plaintext is the compact JSON encoding of the value (§10.5).
 */
type Visit = (value: unknown, field: string) => Promise<unknown>;

/** Chat message or delta members whose whole value is content, both ways (§10.5). */
const MESSAGE_MEMBERS = [
  'content',
  'reasoning',
  'reasoning_content',
  'refusal',
  'annotations',
  'reasoning_details',
  'thinking_blocks',
  'reasoning_items',
  'images',
];

/** Choice members that carry or echo tokens (§10.5). */
const CHOICE_MEMBERS = ['logprobs', 'token_ids', 'prompt_token_ids', 'prompt_logprobs'];

async function visitMember(container: Record<string, unknown>, key: string, field: string, visit: Visit): Promise<void> {
  if (container[key] !== undefined) container[key] = await visit(container[key], field);
}

/** Every location of a chat message or delta, the same set in requests and responses. */
async function visitMessage(message: Record<string, unknown>, prefix: string, visit: Visit): Promise<void> {
  for (const key of MESSAGE_MEMBERS) await visitMember(message, key, `${prefix}.${key}`, visit);
  if (isObject(message.audio)) {
    for (const key of ['data', 'transcript']) await visitMember(message.audio, key, `${prefix}.audio.${key}`, visit);
  }
  if (isObject(message.function_call)) {
    await visitMember(message.function_call, 'arguments', `${prefix}.function_call.arguments`, visit);
  }
  for (const [t, call] of indexed(message.tool_calls)) {
    for (const [kind, key] of [['function', 'arguments'], ['custom', 'input']] as const) {
      const inner = call[kind];
      if (isObject(inner)) await visitMember(inner, key, `${prefix}.tool_calls.${t}.${kind}.${key}`, visit);
    }
  }
}

async function visitMessages(body: Record<string, unknown>, visit: Visit): Promise<void> {
  const messages = Array.isArray(body.messages) ? body.messages : [];
  for (const [m, message] of messages.entries()) {
    if (isObject(message)) await visitMessage(message, `messages.${m}`, visit);
  }
}

/** Token ids, a number or an array of numbers, stay readable in `prompt` and `input` (§5). */
function isTokenIds(value: unknown): boolean {
  return typeof value === 'number' || (Array.isArray(value) && value.every((item) => typeof item === 'number'));
}

/** `prompt` / `input`: the whole value, or each array item, unless it is token ids. */
async function visitTextItems(body: Record<string, unknown>, key: string, visit: Visit): Promise<void> {
  const value = body[key];
  if (Array.isArray(value)) {
    for (const [i, item] of value.entries()) {
      if (!isTokenIds(item)) value[i] = await visit(item, `${key}.${i}`);
    }
  } else if (value !== undefined && !isTokenIds(value)) {
    body[key] = await visit(value, key);
  }
}

async function visitRequest(body: Record<string, unknown>, endpoint: Endpoint, visit: Visit): Promise<void> {
  if (endpoint === 'completions') {
    await visitTextItems(body, 'prompt', visit);
    await visitMember(body, 'suffix', 'suffix', visit);
  } else if (endpoint === 'embeddings') {
    await visitTextItems(body, 'input', visit);
    await visitMessages(body, visit);
  } else {
    if (isObject(body.prediction)) await visitMember(body.prediction, 'content', 'prediction.content', visit);
    await visitMessages(body, visit);
  }
}

/** Every location of one response object; a choice's `message` and `delta` are both visited when present. */
async function visitResponse(payload: Record<string, unknown>, endpoint: Endpoint, visit: Visit): Promise<void> {
  for (const key of ['prompt_token_ids', 'prompt_logprobs']) await visitMember(payload, key, key, visit);
  const list = endpoint === 'embeddings' ? 'data' : 'choices';
  for (const [i, entry] of indexed(payload[list])) {
    const prefix = `${list}.${i}`;
    if (endpoint === 'embeddings') {
      await visitMember(entry, 'embedding', `${prefix}.embedding`, visit);
      continue;
    }
    for (const key of CHOICE_MEMBERS) await visitMember(entry, key, `${prefix}.${key}`, visit);
    if (endpoint === 'completions') {
      await visitMember(entry, 'text', `${prefix}.text`, visit);
    } else {
      for (const part of ['message', 'delta']) {
        if (isObject(entry[part])) await visitMessage(entry[part], `${prefix}.${part}`, visit);
      }
    }
  }
}

// ---- Request fields (§5, §10.5) ---------------------------------------------

/**
 * Encrypt every request content location in place, as the JSON of its value.
 * Every other member is sent as it is: readable, but bound by the body digest
 * (§10.3). Token ids in `prompt` and `input` stay readable as in v2 (§5).
 */
async function sealRequestFields(
  payload: Record<string, unknown>,
  endpoint: Endpoint,
  ctx: RequestContext,
  serviceKey: CryptoKey,
): Promise<void> {
  await visitRequest(payload, endpoint, async (value, field) =>
    value === null
      ? null
      : seal(
          serviceKey,
          new TextEncoder().encode(JSON.stringify(value)),
          fieldAad('aci.e2ee.request.v2.1', ctx, field),
          ctx.secret,
          FIELD_INFO,
        ),
  );
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
  for (const name of ['choices', 'data']) {
    if (payload[name] !== undefined && payload[name] !== null && !Array.isArray(payload[name])) {
      throw new E2eeError(`"${name}" is not an array`);
    }
  }
  const id = typeof payload.id === 'string' ? payload.id : '';
  await visitResponse(payload, endpoint, async (value, field) => {
    if (value === null) return null;
    if (typeof value !== 'string') throw new E2eeError(`${field} is not a ciphertext string`);
    const plaintext = await openBytes(
      ctx.clientPrivateKey,
      value,
      fieldAad('aci.e2ee.response.v2.1', ctx, field, id),
      ctx.secret,
      FIELD_INFO,
    );
    try {
      return JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(plaintext));
    } catch {
      throw new E2eeError(`${field} does not hold JSON`);
    }
  });
}

/**
 * Check and decrypt each `data:` event of an SSE stream (§10.6). `[DONE]`
 * passes through, comments are dropped, and any other line fails the stream.
 * A repeated, reordered, or missing event fails on its `e2ee_seq`; a stream
 * cut short does not (§10.8).
 */
function openSseStream(endpoint: Endpoint, ctx: RequestContext): TransformStream<Uint8Array, Uint8Array> {
  const decoder = new TextDecoder();
  const encoder = new TextEncoder();
  let buffered = '';
  // Every event of one stream carries the same response id and the next
  // position (§10.6).
  let streamId: unknown;
  let nextSeq = 0;
  // `[DONE]` ends the stream; nothing may follow it.
  let done = false;
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
      if (done) throw new E2eeError('stream event after [DONE]');
      const joined = data.join('\n');
      done = joined === '[DONE]';
      let rendered = joined;
      if (joined !== '[DONE]') {
        const payload = await openObject(joined, endpoint, ctx);
        if (payload[SEQ_MEMBER] !== nextSeq) {
          throw new E2eeError(`stream event is out of sequence: expected ${SEQ_MEMBER} ${nextSeq}`);
        }
        delete payload[SEQ_MEMBER];
        nextSeq += 1;
        streamId ??= payload.id;
        if (payload.id !== streamId) {
          // An in-band error event (the gateway's MAC'd failure report) ends
          // the stream; surface its error instead of the id mismatch.
          if (isObject(payload.error)) {
            const { type, message } = payload.error;
            throw new E2eeError(`stream ended with an error event: ${String(type ?? 'error')}: ${String(message ?? '')}`);
          }
          throw new E2eeError('stream event belongs to another response');
        }
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
 * non-negative safe integer, else the position (§10.5). Anything but an array
 * has none.
 */
function indexed(value: unknown): Array<[number, Record<string, unknown>]> {
  if (!Array.isArray(value)) return [];
  return value.flatMap((entry, position): Array<[number, Record<string, unknown>]> => {
    if (!isObject(entry)) return [];
    const index = entry.index;
    return [[typeof index === 'number' && Number.isSafeInteger(index) && index >= 0 ? index : position, entry]];
  });
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
