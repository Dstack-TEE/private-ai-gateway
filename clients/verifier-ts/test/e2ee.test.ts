import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createE2eeFetch, E2eeError, E2eeReverifyError, E2EE_V2_1_X25519, fromHex, sha256Hex, toHex } from '../src/index.js';
import type { ReportTranscript, WorkloadKeyset } from '../src/index.js';
import { canonicalJson, openBytes, openResponse, seal } from '../src/e2ee.js';
import type { RequestContext } from '../src/e2ee.js';

// Constants from spec/e2ee-v2-test-vectors.md, "Version 2.1".
const SERVICE_PUB = '5dfedd3b6bd47f6fa28ee15d969d5bb0ea53774d488bdaf9df1c6e0124b3ef22';
const CLIENT_PUB = 'ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b';
const NONCE = '000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f';
const SECRET = new Uint8Array(32).fill(0x0a);
const FIELD_AAD =
  '{"algo":"x25519-aes-256-gcm-hkdf-sha256-v2.1","field":"messages.0.content","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.request.v2.1","ts":1750000000}';
const FIELD_CIPHERTEXT =
  '50a61409b1ddd0325e9b16b700e719e9772c07000b1bd7786e907c653d20495d0606060606060606060606067c3f905a1e58cb237be461eb622de6935599cbae71e1e1';
const BODY_JCS = `{"messages":[{"content":"${FIELD_CIPHERTEXT}","role":"user"}],"model":"demo-model","temperature":0.7}`;
const BODY_DIGEST = 'dc75693896a5b40b45592667704f41ff057a5063c92e3117d930b98049032da5';
const REQUEST_KEY_AAD =
  '{"algo":"x25519-aes-256-gcm-hkdf-sha256-v2.1","client_pub_key":"ac01b2209e86354fb853237b5de0f4fab13c7fcbf433a61c019369617fecf10b","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.request-key.v2.1","ts":1750000000}';
const REQUEST_KEY =
  '13be4feaeaf204c7fd3358fc9c00721881d174278128227ec674f37f7fe97b6d080808080808080808080808d4c43624763b8e0079b38271adfa9fae2e78243bb0fa918cd95421605c3b82b5bd7ac017cda0e8ac1c9836d409013e2369dd90e254dbbe44006ff09d098c5f212ff666088c3724c5c51b43e51576c633';
const RESPONSE_AAD =
  '{"algo":"x25519-aes-256-gcm-hkdf-sha256-v2.1","field":"choices.0.message.content","id":"chatcmpl-123","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.response.v2.1","ts":1750000000}';
const RESPONSE_CIPHERTEXT =
  '57db4b359f23ae5e146e4e2512056704722506348c150c14753d0c933d04d4210b0b0b0b0b0b0b0b0b0b0b0b7faa9fc1dab7d51b7151db2c848775112941ae28ccdc25655e089f8e';

const RESPONSE_OBJECT = '{"choices":[{"index":0,"message":{"content":"57db4b359f23ae5e146e4e2512056704722506348c150c14753d0c933d04d4210b0b0b0b0b0b0b0b0b0b0b0b7faa9fc1dab7d51b7151db2c848775112941ae28ccdc25655e089f8e","role":"assistant"}}],"id":"chatcmpl-123"}';
const RESPONSE_MAC = '139caafa035982d63cbdcce428a560af4752fcfc31bf91fb3b633ffd193e1d7e';

// Labels written out independently of the implementation (spec §10.4).
const FIELD_INFO = new TextEncoder().encode('aci.e2ee.v2.1.x25519');
const REQUEST_KEY_INFO = new TextEncoder().encode('aci.e2ee.v2.1.x25519.request-key');
const MAC_INFO = new TextEncoder().encode('aci.e2ee.v2.1.response-mac');
const enc = new TextEncoder();
const dec = new TextDecoder();
const subtle = globalThis.crypto.subtle;

/** An X25519 key pair from a fixed 32-byte seed (PKCS #8 import). */
async function x25519FromSeed(byte: number): Promise<CryptoKeyPair> {
  const pkcs8 = new Uint8Array([...fromHex('302e020100300506032b656e04220420'), ...new Uint8Array(32).fill(byte)]);
  const privateKey = await subtle.importKey('pkcs8', pkcs8, { name: 'X25519' }, true, ['deriveBits']);
  const jwk = await subtle.exportKey('jwk', privateKey);
  const raw = Uint8Array.from(atob(jwk.x!.replace(/-/g, '+').replace(/_/g, '/')), (c) => c.charCodeAt(0));
  const publicKey = await subtle.importKey('raw', raw, { name: 'X25519' }, true, []);
  return { privateKey, publicKey };
}

const rawPublic = async (pair: CryptoKeyPair) => toHex(new Uint8Array(await subtle.exportKey('raw', pair.publicKey)));

function keyset(algo = E2EE_V2_1_X25519): WorkloadKeyset {
  return {
    not_after: 1_800_000_000,
    receipt_signing_keys: [],
    e2ee_public_keys: [
      { key_id: 'e2ee-1', algo: 'x25519-aes-256-gcm-hkdf-sha256', public_key: SERVICE_PUB },
      { key_id: 'e2ee-1', algo, public_key: SERVICE_PUB },
    ],
  };
}

const DIGEST = `sha256:${'ab'.repeat(32)}`;

/** A transcript as `verifyService` returns it once id-6 passed with the E2EE v2.1 binding. */
function transcript(ks: WorkloadKeyset = keyset(), overrides: Partial<ReportTranscript> = {}): ReportTranscript {
  return {
    lines: [
      { id: 'id-4', section: '9.1(4)', title: 'provenance', status: 'pass' },
      { id: 'id-6', section: '9.1(6)', title: 'channel', status: 'pass' },
    ],
    verdict: { verified: true, line: 'VERIFIED (6 pass)' },
    verification: { ok: true, checks: [], keyset: ks, workloadKeysetDigest: DIGEST },
    channel: { e2ee: E2EE_V2_1_X25519 },
    ...overrides,
  };
}

test('canonical JSON matches the published v2.1 body and handles integer-like keys', async () => {
  const body = {
    model: 'demo-model',
    temperature: 0.7,
    messages: [{ role: 'user', content: FIELD_CIPHERTEXT }],
  };
  assert.equal(dec.decode(canonicalJson(body)), BODY_JCS);
  assert.equal(await sha256Hex(canonicalJson(body)), BODY_DIGEST);
  // JS objects enumerate integer-like keys first; JCS sorts by code units.
  assert.equal(dec.decode(canonicalJson({ a: 3, 10: 2, 9: 1 })), '{"10":2,"9":1,"a":3}');
  assert.equal(dec.decode(canonicalJson([1e-7, 1e21, -0, 0.1 + 0.2])), '[1e-7,1e+21,0,0.30000000000000004]');
});

test('seal reproduces the published v2.1 ciphertexts byte for byte', async () => {
  const service = await subtle.importKey('raw', fromHex(SERVICE_PUB) as BufferSource, { name: 'X25519' }, true, []);
  const field = await seal(service, enc.encode('"hello"'), enc.encode(FIELD_AAD), SECRET, FIELD_INFO, {
    ephemeral: await x25519FromSeed(0x05),
    nonce: new Uint8Array(12).fill(0x06),
  });
  assert.equal(field, FIELD_CIPHERTEXT);
  const requestKey = await seal(
    service,
    new Uint8Array([...SECRET, ...fromHex(BODY_DIGEST)]),
    enc.encode(REQUEST_KEY_AAD),
    new Uint8Array(0),
    REQUEST_KEY_INFO,
    { ephemeral: await x25519FromSeed(0x07), nonce: new Uint8Array(12).fill(0x08) },
  );
  assert.equal(requestKey, REQUEST_KEY);
});

test('the client opens the published v2.1 response field', async () => {
  const client = await x25519FromSeed(0x04);
  assert.equal(await rawPublic(client), CLIENT_PUB);
  const plaintext = await openBytes(client.privateKey, RESPONSE_CIPHERTEXT, enc.encode(RESPONSE_AAD), SECRET, FIELD_INFO);
  assert.equal(dec.decode(plaintext), 'plain-answer');
  // Without the request secret the client's own key opens nothing.
  await assert.rejects(
    openBytes(client.privateKey, RESPONSE_CIPHERTEXT, enc.encode(RESPONSE_AAD), new Uint8Array(32), FIELD_INFO),
    E2eeError,
  );
});

test('the published v2.1 response MAC reproduces', async () => {
  assert.equal(JSON.parse(await withMac(JSON.parse(RESPONSE_OBJECT), SECRET)).e2ee_mac, RESPONSE_MAC);
});

test('no client without a VERIFIED transcript bound to E2EE v2.1 and a v2.1 key', () => {
  assert.throws(() => createE2eeFetch({ transcript: transcript(keyset('x25519-aes-256-gcm-hkdf-sha256')) }), E2eeError);
  const refused: Partial<ReportTranscript>[] = [
    { verdict: { verified: false, line: 'PARTIAL — hardware root not verified' } },
    { channel: { observedSpkiSha256: 'ab'.repeat(32) } },
    { channel: { e2ee: 'x25519-aes-256-gcm-hkdf-sha256' } },
    { lines: [{ id: 'id-6', section: '9.1(6)', title: 'channel', status: 'fail' }] },
    // Offline, a report without app_compose skips id-4 and still verifies;
    // its id-6 binding then pins no compose.
    {
      lines: [
        { id: 'id-4', section: '9.1(4)', title: 'provenance', status: 'skip' },
        { id: 'id-6', section: '9.1(6)', title: 'channel', status: 'pass' },
      ],
    },
  ];
  for (const overrides of refused) {
    assert.throws(() => createE2eeFetch({ transcript: transcript(keyset(), overrides) }), /must be VERIFIED/);
  }
});

// ---- A test-only service: just enough of spec §10.6 to drive the client ------

interface Seen {
  headers: Headers;
  plaintext: Record<string, unknown>;
}

function aad(fields: Record<string, unknown>): Uint8Array {
  return canonicalJson({ algo: E2EE_V2_1_X25519, ...fields });
}

/** Open a v2.1 request the way the gateway does, then reply with `reply(plaintext)`. */
/** HMAC-SHA256 under HKDF(ikm = secret, info = response-mac) over the object's JCS (spec §10.6). */
async function withMac(object: Record<string, unknown>, secret: Uint8Array): Promise<string> {
  const ikm = await subtle.importKey('raw', secret as BufferSource, 'HKDF', false, ['deriveKey']);
  const key = await subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: new Uint8Array(0), info: MAC_INFO },
    ikm,
    { name: 'HMAC', hash: 'SHA-256', length: 256 },
    false,
    ['sign'],
  );
  const mac = toHex(new Uint8Array(await subtle.sign('HMAC', key, canonicalJson(object) as BufferSource)));
  return JSON.stringify({ ...object, e2ee_mac: mac });
}

type Tamper = (wire: string) => string;

function fakeService(
  reply: (plaintext: Record<string, unknown>) => { body: string; contentType: string; headers?: Record<string, string> },
  seen: Seen[],
  tamper: Tamper = (wire) => wire,
) {
  return async (input: RequestInfo | URL, init?: RequestInit): Promise<Response> => {
    const service = await x25519FromSeed(0x03);
    const headers = new Headers(init?.headers);
    const body = JSON.parse(String(init?.body)) as Record<string, any>;
    const nonce = headers.get('x-e2ee-nonce')!;
    const ts = Number(headers.get('x-e2ee-timestamp'));
    const client = headers.get('x-client-pub-key')!;
    const opened = await openBytes(
      service.privateKey,
      headers.get('x-e2ee-request-key')!,
      aad({ purpose: 'aci.e2ee.request-key.v2.1', model: body.model, client_pub_key: client, nonce, ts }),
      new Uint8Array(0),
      REQUEST_KEY_INFO,
    );
    const secret = opened.subarray(0, 32);
    assert.equal(toHex(opened.subarray(32)), await sha256Hex(canonicalJson(body)));
    const open = async (field: string, ciphertext: string) =>
      dec.decode(
        await openBytes(service.privateKey, ciphertext, aad({ purpose: 'aci.e2ee.request.v2.1', model: body.model, field, nonce, ts }), secret, FIELD_INFO),
      );
    const openString = async (container: Record<string, any> | undefined, key: string, field: string) => {
      if (typeof container?.[key] === 'string') container[key] = await open(field, container[key]);
    };
    if (typeof body.prediction?.content === 'string') {
      body.prediction.content = JSON.parse(await open('prediction.content', body.prediction.content));
    }
    for (const [m, message] of (body.messages ?? []).entries()) {
      if (typeof message.content === 'string') message.content = JSON.parse(await open(`messages.${m}.content`, message.content));
      for (const key of ['reasoning', 'reasoning_content', 'refusal']) await openString(message, key, `messages.${m}.${key}`);
      await openString(message.function_call, 'arguments', `messages.${m}.function_call.arguments`);
      for (const [t, call] of (message.tool_calls ?? []).entries()) {
        await openString(call.function, 'arguments', `messages.${m}.tool_calls.${t}.function.arguments`);
      }
    }
    for (const key of ['prompt', 'input']) {
      if (Array.isArray(body[key])) {
        for (const [i, item] of body[key].entries()) await openString(body[key], String(i), `${key}.${i}`);
      } else {
        await openString(body, key, key);
      }
    }
    await openString(body, 'suffix', 'suffix');
    seen.push({ headers, plaintext: body });

    const clientKey = await subtle.importKey('raw', fromHex(client) as BufferSource, { name: 'X25519' }, true, []);
    const sealResponse = (id: string, field: string, plaintext: string) =>
      seal(clientKey, enc.encode(plaintext), aad({ purpose: 'aci.e2ee.response.v2.1', model: body.model, id, field, nonce, ts }), secret, FIELD_INFO);
    const out = reply(body);
    const sealed = tamper(await sealSse(out.body, out.contentType, sealResponse, secret));
    return new Response(sealed, {
      headers: { 'content-type': out.contentType, 'x-e2ee-applied': 'true', 'x-e2ee-version': '2.1', ...out.headers },
    });
  };
}

/** Encrypt the content fields of a buffered body or of each SSE `data:` event. */
async function sealSse(
  body: string,
  contentType: string,
  sealResponse: (id: string, field: string, plaintext: string) => Promise<string>,
  secret: Uint8Array,
): Promise<string> {
  const sealPayload = async (payload: Record<string, any>, part: 'message' | 'delta') => {
    for (const choice of payload.choices ?? []) {
      const target = choice[part];
      if (typeof target?.content === 'string') {
        target.content = await sealResponse(payload.id, `choices.${choice.index}.${part}.content`, target.content);
      }
      const prefix = `choices.${choice.index}.${part}`;
      for (const key of ['refusal', 'audio.data', 'audio.transcript']) {
        const [outer, inner] = key.includes('.') ? [target?.audio, key.split('.')[1]!] : [target, key];
        if (typeof outer?.[inner] === 'string') outer[inner] = await sealResponse(payload.id, `${prefix}.${key}`, outer[inner]);
      }
      if (target?.annotations !== undefined) {
        target.annotations = await sealResponse(payload.id, `${prefix}.annotations`, JSON.stringify(target.annotations));
      }
      if (typeof choice.text === 'string') choice.text = await sealResponse(payload.id, `choices.${choice.index}.text`, choice.text);
      if (choice.token_ids !== undefined) {
        choice.token_ids = await sealResponse(payload.id, `choices.${choice.index}.token_ids`, JSON.stringify(choice.token_ids));
      }
      for (const [t, call] of (target?.tool_calls ?? []).entries()) {
        const index = call.index ?? t;
        call.function.arguments = await sealResponse(
          payload.id,
          `choices.${choice.index}.${part}.tool_calls.${index}.function.arguments`,
          call.function.arguments,
        );
      }
    }
    return withMac(payload, secret);
  };
  if (!contentType.startsWith('text/event-stream')) return sealPayload(JSON.parse(body), 'message');
  const events = [];
  let seq = 0;
  for (const event of body.split('\n\n').filter(Boolean)) {
    const data = event.slice('data: '.length);
    events.push(data === '[DONE]' ? event : `data: ${await sealPayload({ ...JSON.parse(data), e2ee_seq: seq++ }, 'delta')}`);
  }
  return events.map((event) => event + '\n\n').join('');
}

test('chat round trip: the service sees plaintext, the caller gets it back, nothing in between does', async () => {
  const seen: Seen[] = [];
  const e2eeFetch = createE2eeFetch({
    transcript: transcript(),
    fetch: fakeService(
      () => ({
        contentType: 'application/json',
        body: JSON.stringify({
          id: 'chat-1',
          choices: [
            {
              index: 0,
              message: {
                role: 'assistant',
                content: 'answer',
                tool_calls: [{ id: 'c1', type: 'function', function: { name: 'lookup', arguments: '{"q":1}' } }],
              },
            },
          ],
        }),
      }),
      seen,
    ),
  });

  const request = {
    model: 'demo-model',
    logit_bias: { '50256': -100, '9': 1 },
    messages: [
      { role: 'user', content: '[1, 2]' },
      { role: 'assistant', content: null, tool_calls: [{ id: 'c0', type: 'function', function: { name: 'f', arguments: '{"a":1}' } }] },
      { role: 'tool', tool_call_id: 'c0', content: [{ type: 'text', text: 'ok' }] },
      { role: 'assistant', content: 'done', reasoning_content: 'earlier private thought' },
      // A reply sent back as history: its null members count as absent.
      { role: 'assistant', content: 'again', refusal: null, tool_calls: null, annotations: null, audio: null, function_call: null },
      { role: 'user', content: 'thanks' },
    ],
  };
  const response = await e2eeFetch('https://relay.example/api/v1/chat/completions', {
    method: 'POST',
    headers: { authorization: 'Bearer k', 'x-signing-algo': 'ecdsa' },
    body: JSON.stringify(request),
  });

  assert.equal(response.status, 200);
  const result = (await response.json()) as any;
  assert.equal(result.choices[0].message.content, 'answer');
  assert.equal(result.choices[0].message.tool_calls[0].function.arguments, '{"q":1}');

  const [{ headers, plaintext }] = seen as [Seen];
  assert.deepEqual(plaintext.messages, request.messages);
  assert.equal(headers.get('x-e2ee-version'), '2.1');
  assert.equal(headers.get('authorization'), 'Bearer k');
  assert.equal(headers.get('x-signing-algo'), null);
});

test('streamed chat is decrypted event by event across chunk boundaries', async () => {
  const sse = [
    { id: 's1', choices: [{ index: 0, delta: { content: 'Hel' } }] },
    { id: 's1', choices: [{ index: 0, delta: { tool_calls: [{ index: 2, function: { arguments: '{"x"' } }] } }] },
  ]
    .map((event) => `data: ${JSON.stringify(event)}\n\n`)
    .join('') + 'data: [DONE]\n\n';
  const inner = fakeService(() => ({ contentType: 'text/event-stream', body: sse }), []);
  // Re-chunk the service's stream into 7-byte pieces.
  const chunked = async (input: RequestInfo | URL, init?: RequestInit) => {
    const response = await inner(input, init);
    const bytes = new Uint8Array(await response.arrayBuffer());
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        for (let i = 0; i < bytes.length; i += 7) controller.enqueue(bytes.subarray(i, i + 7));
        controller.close();
      },
    });
    return new Response(body, { headers: response.headers });
  };
  const e2eeFetch = createE2eeFetch({ transcript: transcript(), fetch: chunked });
  const response = await e2eeFetch('https://relay.example/v1/chat/completions', {
    method: 'POST',
    body: JSON.stringify({ model: 'demo-model', stream: true, messages: [{ role: 'user', content: 'hi' }] }),
  });
  const events = (await response.text()).split('\n\n').filter(Boolean);
  assert.deepEqual(JSON.parse(events[0]!.slice(6)).choices[0].delta, { content: 'Hel' });
  assert.equal(JSON.parse(events[1]!.slice(6)).choices[0].delta.tool_calls[0].function.arguments, '{"x"');
  assert.equal(events[2], 'data: [DONE]');
});

const chatReply = () => ({
  contentType: 'application/json',
  body: JSON.stringify({
    id: 'chat-2',
    choices: [
      {
        index: 0,
        message: { role: 'assistant', content: 'ok', tool_calls: [{ id: 'c', type: 'function', function: { name: 'lookup', arguments: '{}' } }] },
        finish_reason: 'tool_calls',
      },
    ],
  }),
});
const ask = (f: ReturnType<typeof createE2eeFetch>, extra: Record<string, unknown> = {}) =>
  f('https://relay.example/v1/chat/completions', {
    method: 'POST',
    body: JSON.stringify({ model: 'demo-model', messages: [{ role: 'user', content: 'hi' }], ...extra }),
  });

test('the MAC covers readable members: a relay cannot rename a tool call', async () => {
  const honest = createE2eeFetch({ transcript: transcript(), fetch: fakeService(chatReply, []) });
  assert.equal((await (await ask(honest)).json() as any).choices[0].message.tool_calls[0].function.name, 'lookup');
  const renamed = createE2eeFetch({
    transcript: transcript(),
    fetch: fakeService(chatReply, [], (wire) => wire.replace('"lookup"', '"delete_account"')),
  });
  await assert.rejects(ask(renamed), /MAC/);
});

test('sends stop once the keyset expires or the service names another keyset', async () => {
  const expired = createE2eeFetch({
    transcript: transcript({ ...keyset(), not_after: Math.floor(Date.now() / 1000) }),
    fetch: fakeService(chatReply, []),
  });
  await assert.rejects(ask(expired), E2eeReverifyError);

  const sends: string[] = [];
  const served = (digest: string) => async (input: RequestInfo | URL, init?: RequestInit) => {
    sends.push(digest);
    const response = await fakeService(chatReply, [])(input, init);
    const headers = new Headers(response.headers);
    headers.set('x-aci-keyset-digest', digest);
    return new Response(response.body, { status: response.status, headers });
  };
  const same = createE2eeFetch({ transcript: transcript(), fetch: served(DIGEST) });
  await ask(same);
  await ask(same);
  const renewed = `sha256:${'cd'.repeat(32)}`;
  const moved = createE2eeFetch({ transcript: transcript(), fetch: served(renewed) });
  // The response made for the verified key still opens; the next send does not go out.
  assert.equal(((await (await ask(moved)).json()) as any).choices[0].message.content, 'ok');
  await assert.rejects(ask(moved), E2eeReverifyError);
  assert.deepEqual(sends, [DIGEST, DIGEST, renewed]);
});

test('fails closed on uncovered endpoints and anything not v2.1', async () => {
  const passthrough = createE2eeFetch({
    transcript: transcript(),
    fetch: async (input, init) => new Response(`${init?.method ?? new Request(input).method}`),
  });
  assert.equal(await (await passthrough('https://relay.example/v1/models')).text(), 'GET');
  await assert.rejects(
    passthrough('https://relay.example/v1/responses', { method: 'POST', body: '{"model":"m"}' }),
    E2eeError,
  );
  const forged = JSON.stringify({ id: 'x', choices: [{ index: 0, message: { content: 'FORGED' } }] });
  const reply = (body: string, headers: Record<string, string>) =>
    createE2eeFetch({ transcript: transcript(), fetch: async () => new Response(body, { headers }) });
  await assert.rejects(ask(reply(forged, { 'x-e2ee-applied': 'false' })), E2eeError);
  await assert.rejects(ask(reply(forged, { 'content-type': 'application/json' })), /e2ee_mac/);
  // A JSON body labelled as a stream for a buffered request.
  await assert.rejects(ask(reply(forged, { 'content-type': 'text/event-stream' })), /expected a JSON body/);
  // `choices` that is not an array, even with a MAC field present.
  const objectChoices = JSON.stringify({ id: 'x', choices: { 0: { message: { content: 'x' } } }, e2ee_mac: '0'.repeat(64) });
  await assert.rejects(ask(reply(objectChoices, { 'content-type': 'application/json' })), E2eeError);

  // In a stream, a line that is not `data:` or a comment fails the stream.
  const streamed = createE2eeFetch({
    transcript: transcript(),
    fetch: fakeService(
      () => ({ contentType: 'text/event-stream', body: `data: ${JSON.stringify({ id: 's', choices: [{ index: 0, delta: { content: 'a' } }] })}\n\ndata: [DONE]\n\n` }),
      [],
      (wire) => ': keep-alive\n\n' + wire + ' data: {"choices":[{"delta":{"content":"FORGED"}}]}\n\n',
    ),
  });
  const response = await ask(streamed, { stream: true });
  await assert.rejects(response.text(), /unexpected line/);

  // Events of one stream must share one response id.
  const spliced = createE2eeFetch({
    transcript: transcript(),
    fetch: fakeService(
      () => ({
        contentType: 'text/event-stream',
        body: ['s1', 's2'].map((id) => `data: ${JSON.stringify({ id, choices: [{ index: 0, delta: { content: id } }] })}\n\n`).join(''),
      }),
      [],
    ),
  });
  await assert.rejects((await ask(spliced, { stream: true })).text(), /another response/);

  // A mid-stream error event without an id still fails, with its error.
  const failed = createE2eeFetch({
    transcript: transcript(),
    fetch: fakeService(
      () => ({
        contentType: 'text/event-stream',
        body: [
          { id: 's', choices: [{ index: 0, delta: { content: 'a' } }] },
          { error: { type: 'upstream_error', message: 'upstream failed' } },
        ].map((event) => `data: ${JSON.stringify(event)}\n\n`).join(''),
      }),
      [],
    ),
  });
  await assert.rejects(
    (await ask(failed, { stream: true })).text(),
    (err: unknown) => err instanceof E2eeError && /error event: upstream_error: upstream failed/.test(err.message),
  );
});

// ---- Published streamed vector and the streams a client must reject ----------

const EVENT_1 = '{"choices":[{"delta":{"content":"97c3b10b4d6c133a78ea5dcc1cf6421d3f81ae37b1f628ce14ca6fce7730f3330d0d0d0d0d0d0d0d0d0d0d0df4e9f89190a3ecafd2eb8d3b91503afa4780983433"},"index":0}],"e2ee_mac":"5c37140c3570da5ce27bb6442265d2c1bc452824d12d06d1a59e11f3a01f1b44","e2ee_seq":0,"id":"chatcmpl-123"}';
const EVENT_2 = '{"choices":[{"delta":{},"finish_reason":"stop","index":0}],"e2ee_mac":"7d11e2d261802a4a90f15a70727e541c1972168c6e053dd7ef5d4e6fe3b5cd7c","e2ee_seq":1,"id":"chatcmpl-123"}';
const SPLICED_2 = '{"choices":[{"delta":{},"finish_reason":"stop","index":0}],"e2ee_mac":"bc1bf0a5179c96234d1533533f31f7d2fc5471004c2b7f7b10d37e4d94bb24fe","e2ee_seq":1,"id":"chatcmpl-124"}';
const stream = (...events: string[]) => events.map((event) => `data: ${event}\n\n`).join('') + 'data: [DONE]\n\n';

/** The request context of the published vectors (fixed secret, nonce, and client key). */
async function vectorContext(streamed: boolean): Promise<RequestContext> {
  const client = await x25519FromSeed(0x04);
  const ikm = await subtle.importKey('raw', SECRET as BufferSource, 'HKDF', false, ['deriveKey']);
  const macKey = await subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: new Uint8Array(0), info: MAC_INFO },
    ikm,
    { name: 'HMAC', hash: 'SHA-256', length: 256 },
    false,
    ['verify'],
  );
  return {
    model: 'demo-model',
    nonce: NONCE,
    ts: 1_750_000_000,
    secret: SECRET,
    clientPrivateKey: client.privateKey,
    clientPublicKeyHex: CLIENT_PUB,
    macKey,
    stream: streamed,
  };
}

const served = (body: string, contentType: string) => new Response(body, { headers: { 'content-type': contentType } });

test('the published v2.1 stream opens, and each must-reject variant is refused', async () => {
  const opened = await openResponse(served(stream(EVENT_1, EVENT_2), 'text/event-stream'), 'chat', await vectorContext(true));
  const events = (await opened.text()).split('\n\n').filter(Boolean);
  assert.deepEqual(JSON.parse(events[0]!.slice(6)), { id: 'chatcmpl-123', choices: [{ index: 0, delta: { content: 'Hello' } }] });
  assert.equal(JSON.parse(events[1]!.slice(6)).choices[0].finish_reason, 'stop');
  assert.equal(events[2], 'data: [DONE]');

  const rejects = async (body: string, contentType: string, why: RegExp) => {
    const ctx = await vectorContext(true);
    await assert.rejects(async () => (await openResponse(served(body, contentType), 'chat', ctx)).text(), why);
  };
  await rejects(stream(EVENT_1, EVENT_2.replace('"finish_reason":"stop"', '"finish_reason":"length"')), 'text/event-stream', /MAC/);
  await rejects(stream(EVENT_1, SPLICED_2), 'text/event-stream', /another response/);
  await rejects(stream(EVENT_1.replace(/,"e2ee_mac":"[0-9a-f]+"/, ''), EVENT_2), 'text/event-stream', /e2ee_mac/);
  await rejects('id: 1\n' + stream(EVENT_1, EVENT_2), 'text/event-stream', /unexpected line/);
  // Each event is validly MAC'd, but not in its position.
  await rejects(stream(EVENT_1, EVENT_1, EVENT_2), 'text/event-stream', /out of sequence/);
  await rejects(stream(EVENT_2, EVENT_1), 'text/event-stream', /out of sequence/);
  await rejects(stream(EVENT_2), 'text/event-stream', /out of sequence/);
  await rejects(stream(EVENT_1) + `data: ${EVENT_2}\n\n`, 'text/event-stream', /after \[DONE\]/);
  const buffered = JSON.stringify({ ...JSON.parse(RESPONSE_OBJECT), e2ee_mac: RESPONSE_MAC });
  await rejects(buffered, 'application/json', /expected an SSE stream/);

  // The same buffered object opens when the request did not ask for a stream.
  const body = await (await openResponse(served(buffered, 'application/json'), 'chat', await vectorContext(false))).json() as any;
  assert.equal(body.choices[0].message.content, 'plain-answer');
});

test('a client that ignores e2ee_seq still verifies and opens the published stream', async () => {
  // The pre-hardening v2.1 client: remove only e2ee_mac, check it over the rest
  // (e2ee_seq included), then decrypt. The current gateway's stream must pass.
  const client = await x25519FromSeed(0x04);
  const contents = [];
  for (const event of [EVENT_1, EVENT_2]) {
    const { e2ee_mac: mac, ...rest } = JSON.parse(event);
    assert.equal(JSON.parse(await withMac(rest, SECRET)).e2ee_mac, mac);
    const content = rest.choices[0].delta.content;
    if (content === undefined) continue;
    const aad = canonicalJson({
      algo: E2EE_V2_1_X25519, field: 'choices.0.delta.content', id: rest.id, model: 'demo-model',
      nonce: NONCE, purpose: 'aci.e2ee.response.v2.1', ts: 1_750_000_000,
    });
    contents.push(dec.decode(await openBytes(client.privateKey, content, aad, SECRET, FIELD_INFO)));
  }
  assert.deepEqual(contents, ['Hello']);
});

/** A fake service that also records the exact bodies sent on the wire. */
function recorded(reply: Parameters<typeof fakeService>[0]) {
  const seen: Seen[] = [];
  const wire: string[] = [];
  const service = fakeService(reply, seen);
  const fetch = (input: RequestInfo | URL, init?: RequestInit) => {
    wire.push(String(init?.body));
    return service(input, init);
  };
  return { e2eeFetch: createE2eeFetch({ transcript: transcript(), fetch }), seen, wire };
}

test('every member a plaintext request can carry is sent, its content encrypted', async () => {
  const { e2eeFetch, seen, wire } = recorded(chatReply);
  const request = {
    model: 'demo-model',
    thinking: { type: 'enabled', budget_tokens: 512 },
    chat_template_kwargs: { enable_thinking: false },
    include_reasoning: true,
    prompt_cache_key: 'conversation-7',
    metadata: { tenant: 't-1' },
    x_vendor_unknown: [1, { a: null }],
    functions: [{ name: 'lookup', parameters: {} }],
    prediction: { type: 'content', content: 'private draft' },
    messages: [
      { role: 'user', content: 'private question' },
      // A reply echoed back as history by the OpenAI SDK.
      {
        role: 'assistant', content: null, annotations: [], audio: null,
        refusal: 'private refusal', function_call: { name: 'lookup', arguments: '{"private":1}' },
      },
    ],
  };
  const response = await e2eeFetch('https://relay.example/v1/chat/completions', { method: 'POST', body: JSON.stringify(request) });
  assert.equal(response.status, 200);
  assert.deepEqual(seen[0]!.plaintext, request);
  assert.ok(!wire[0]!.includes('private'));
  assert.ok(wire[0]!.includes('"prompt_cache_key":"conversation-7"'));
});

test('completions encrypt the suffix and pass a token-id prompt through', async () => {
  const { e2eeFetch, seen, wire } = recorded(() => ({
    contentType: 'application/json',
    body: JSON.stringify({ id: 'cmpl-1', choices: [{ index: 0, text: 'x', token_ids: [7, 8] }] }),
  }));
  const post = (body: Record<string, unknown>) =>
    e2eeFetch('https://relay.example/v1/completions', { method: 'POST', body: JSON.stringify({ model: 'demo-model', ...body }) });
  const reply = (await (await post({ prompt: 'def private(', suffix: 'return secret' })).json()) as any;
  assert.deepEqual(reply.choices[0], { index: 0, text: 'x', token_ids: [7, 8] });
  assert.deepEqual(seen[0]!.plaintext, { model: 'demo-model', prompt: 'def private(', suffix: 'return secret' });
  assert.ok(!wire[0]!.includes('private') && !wire[0]!.includes('secret'));

  await post({ prompt: [[1, 2, 3], [4]] });
  assert.deepEqual(seen[1]!.plaintext.prompt, [[1, 2, 3], [4]]);
});

test('audio, annotations, and refusals in a reply open, buffered and streamed', async () => {
  const annotations = [{ type: 'url_citation', url_citation: { start_index: 0, end_index: 2, url: 'https://example.com/p', title: 'P' } }];
  const generated = { refusal: 'no', annotations, audio: { id: 'a1', data: 'UklGRg==', transcript: 'spoken' } };
  const buffered = recorded(() => ({
    contentType: 'application/json',
    body: JSON.stringify({ id: 'chat-a', choices: [{ index: 0, message: { role: 'assistant', content: 'hi', ...generated } }] }),
  }));
  const message = ((await (await ask(buffered.e2eeFetch)).json()) as any).choices[0].message;
  assert.deepEqual(message, { role: 'assistant', content: 'hi', ...generated });

  const sse = `data: ${JSON.stringify({ id: 's-a', choices: [{ index: 0, delta: generated }] })}\n\ndata: [DONE]\n\n`;
  const streamed = recorded(() => ({ contentType: 'text/event-stream', body: sse }));
  const events = (await (await ask(streamed.e2eeFetch, { stream: true })).text()).split('\n\n').filter(Boolean);
  assert.deepEqual(JSON.parse(events[0]!.slice(6)).choices[0].delta, generated);
});
