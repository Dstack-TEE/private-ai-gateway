import assert from 'node:assert/strict';
import { once } from 'node:events';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { MAX_ATTESTATION_RESPONSE_BYTES, readAciJson } from '../src/attestation-http.js';
import { verifyService } from '../src/index.js';
import { connectAci } from '../src/node/index.js';

test('bounded attestation reader accepts recorded evidence', async () => {
  const text = readFileSync(new URL('../../test/fixtures/aci_report.json', import.meta.url), 'utf8');
  assert.deepEqual(await readAciJson(new Response(text)), JSON.parse(text));
});

test('aborting a stalled body cancels the bounded reader', async () => {
  let cancelled = false;
  const response = new Response(new ReadableStream<Uint8Array>({
    cancel() { cancelled = true; },
  }));
  const controller = new AbortController();
  const reading = readAciJson(response, controller.signal);
  controller.abort(new Error('stalled body aborted'));
  await assert.rejects(reading, /stalled body aborted/);
  assert.equal(cancelled, true);
  assert.equal(response.body?.locked, false);
});

test('connectAci and verifyService deadlines remain active after headers arrive', { timeout: 2000 }, async (t) => {
  let headersSent = false;
  const server = createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.write('{'); // Headers arrive immediately; the body never finishes.
    headersSent = true;
  });
  t.after(() => new Promise<void>((resolve, reject) => {
    server.close((error) => error ? reject(error) : resolve());
    server.closeAllConnections();
  }));
  server.listen(0, '127.0.0.1');
  await once(server, 'listening');
  const address = server.address();
  assert.ok(address && typeof address !== 'string');
  const fetchImpl = Object.assign(
    (_input: RequestInfo | URL, init?: RequestInit) => fetch(`http://127.0.0.1:${address.port}`, init),
    { preconnect: globalThis.fetch.preconnect },
  );
  await assert.rejects(connectAci({
    baseURL: 'https://gateway.example',
    timeoutMs: 100,
    bootstrapFetch: fetchImpl,
  }), /timed out/);
  await assert.rejects(verifyService('https://gateway.example', {
    timeoutMs: 100,
    fetchImpl,
  }), /timed out/);
  assert.equal(headersSent, true);
});

test('many one-byte chunks decode correctly and still enforce the body cap', async () => {
  const expected = { padding: 'x'.repeat(256 * 1024), unicode: 'é🚀' };
  const encoded = new TextEncoder().encode(JSON.stringify(expected));
  for (const overflow of [false, true]) {
    let position = 0;
    let cancelled = false;
    const response = new Response(new ReadableStream<Uint8Array>({
      pull(controller) {
        if (position < encoded.length) {
          controller.enqueue(encoded.slice(position, position + 1));
          position++;
        } else if (overflow && position === encoded.length) {
          position++;
          controller.enqueue(new Uint8Array(MAX_ATTESTATION_RESPONSE_BYTES - encoded.length + 1));
        } else {
          controller.close();
        }
      },
      cancel() { cancelled = true; },
    }));
    if (overflow) {
      await assert.rejects(readAciJson(response), /response exceeds maximum byte size/);
      assert.equal(cancelled, true);
    } else {
      assert.deepEqual(await readAciJson(response), expected);
    }
  }
});

test('both attestation fetch paths reject oversized Content-Length before decoding', async () => {
  let cancellations = 0;
  const fetchImpl = Object.assign(
    async () => new Response(new ReadableStream<Uint8Array>({
      cancel() { cancellations++; },
    }), { headers: { 'content-length': String(MAX_ATTESTATION_RESPONSE_BYTES + 1) } }),
    { preconnect: globalThis.fetch.preconnect },
  );
  await assert.rejects(
    verifyService('https://gateway.example', { fetchImpl }),
    /response exceeds maximum byte size/,
  );
  await assert.rejects(
    connectAci({ baseURL: 'https://gateway.example', bootstrapFetch: fetchImpl }),
    /response exceeds maximum byte size/,
  );
  assert.equal(cancellations, 2);
});

test('attestation body cap also stops streams with absent or understated lengths', async () => {
  for (const headers of [undefined, { 'content-length': '2' }]) {
    let cancelled = false;
    const chunk = new Uint8Array(1024 * 1024);
    const response = new Response(
      new ReadableStream<Uint8Array>({
        pull(controller) { controller.enqueue(chunk); },
        cancel() { cancelled = true; },
      }),
      headers ? { headers } : {},
    );
    await assert.rejects(readAciJson(response), /response exceeds maximum byte size/);
    assert.equal(cancelled, true);
  }
});
