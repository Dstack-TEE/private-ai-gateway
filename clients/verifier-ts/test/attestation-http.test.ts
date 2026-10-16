import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { MAX_ATTESTATION_RESPONSE_BYTES, readAttestationReport } from '../src/attestation-http.js';
import { verifyService } from '../src/index.js';
import { connectAci } from '../src/node/index.js';

test('bounded attestation reader accepts recorded evidence', async () => {
  const text = readFileSync(new URL('../../test/fixtures/aci_report.json', import.meta.url), 'utf8');
  assert.deepEqual(await readAttestationReport(new Response(text)), JSON.parse(text));
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
    await assert.rejects(readAttestationReport(response), /response exceeds maximum byte size/);
    assert.equal(cancelled, true);
  }
});
