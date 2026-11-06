import assert from 'node:assert/strict';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { test } from 'node:test';
import { MAX_ATTESTATION_RESPONSE_BYTES, readAciJson } from '../src/attestation-http.js';
import { verifyService } from '../src/index.js';
import { connectAci } from '../src/node/index.js';

test('bounded JSON reader preserves UTF-8 across chunks', async () => {
  const encoded = new TextEncoder().encode('{"value":"é🚀"}');
  const body = new ReadableStream<Uint8Array>({
    start(controller) {
      for (const byte of encoded) controller.enqueue(Uint8Array.of(byte));
      controller.close();
    },
  });
  assert.deepEqual(await readAciJson(new Response(body)), { value: 'é🚀' });
});

test('attestation fetches reject oversized declared and streamed bodies', async () => {
  for (const length of [undefined, '2', String(MAX_ATTESTATION_RESPONSE_BYTES + 1)]) {
    for (const runtime of [false, true]) {
      let cancelled = false;
      const chunk = new Uint8Array(1024 * 1024);
      const fetchImpl = Object.assign(async () => new Response(new ReadableStream<Uint8Array>({
        pull(controller) { controller.enqueue(chunk); },
        cancel() { cancelled = true; },
      }), { headers: length === undefined ? {} : { 'content-length': length } }), {
        preconnect: globalThis.fetch.preconnect,
      });
      await assert.rejects(
        runtime
          ? connectAci({ baseURL: 'https://gateway.example', bootstrapFetch: fetchImpl })
          : verifyService('https://gateway.example', { fetchImpl }),
        /response exceeds maximum byte size/,
      );
      assert.equal(cancelled, true);
    }
  }
});

test('attestation deadlines remain active after headers arrive', { timeout: 3000 }, async (t) => {
  let headersSent = false;
  const server = createServer((_request, response) => {
    response.writeHead(200, { 'content-type': 'application/json' });
    response.write('{');
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
    baseURL: 'https://gateway.example', timeoutMs: 100, bootstrapFetch: fetchImpl,
  }), /timed out/);
  await assert.rejects(verifyService('https://gateway.example', {
    timeoutMs: 100, fetchImpl,
  }), /timed out/);
  assert.equal(headersSent, true);
});
