import assert from 'node:assert/strict';
import { test } from 'node:test';

import { computeKeysetDigest, hashBody } from '../src/index.js';
import { connectAciWithTransport } from '../src/runtime/connection.js';
import { AciConnectionError, type VerifiedAciIdentity } from '../src/runtime/types.js';
import type { PinnedTransport, PinnedTransportFactory } from '../src/runtime/transport.js';
import type { AttestationReport, ReceiptEnvelope, WorkloadKeyset } from '../src/types.js';
import * as fx from './fixtures.js';

const audioPath = '/v1/audio/transcriptions';
const multipartType = 'multipart/form-data; boundary=audio-test';
function multipartBody(part = 'file'): Uint8Array {
  return Buffer.concat([
    Buffer.from(`--audio-test\r\nContent-Disposition: form-data; name="model"\r\n\r\nm\r\n--audio-test\r\nContent-Disposition: form-data; name="${part}"; filename="provider.wav"\r\nContent-Type: audio/wav\r\n\r\n`),
    Buffer.from([0, 255, 13, 10]),
    Buffer.from('--audio-test--\r\n'),
  ]);
}

function identity(report: AttestationReport, keyset: WorkloadKeyset, digest: string): VerifiedAciIdentity {
  return {
    origin: 'https://api.example.com',
    hostname: 'api.example.com',
    report,
    keyset,
    workloadKeysetDigest: digest,
    composeHash: 'compose',
    tlsSpkiPins: ['c0'.repeat(32)],
    verifiedAt: 1,
    expiresAt: keyset.not_after * 1000,
    transcript: {
      lines: [{ id: 'id-6', section: '9.1(6)', title: 'channel', status: 'pass' }],
      verdict: { verified: true, line: 'VERIFIED' },
      verification: { ok: true, checks: [], workloadKeysetDigest: digest, keyset },
    },
  };
}

function fakeTransportFactory(receipt: ReceiptEnvelope, responseDigest: string, responseDigests?: readonly string[]): {
  factory: PinnedTransportFactory;
  transports: PinnedTransport[];
  closed: boolean[];
} {
  const transports: PinnedTransport[] = [];
  const closed: boolean[] = [];
  const factory: PinnedTransportFactory = () => {
    const index = transports.length;
    const transport: PinnedTransport = {
      fetch: async (input) => {
        const url = input instanceof Request ? input.url : input.toString();
        const path = new URL(url).pathname;
        if (path.endsWith('/attestation')) return new Response('{}');
        if (path.includes('/receipts/')) return Response.json(receipt);
        if (path.includes('/sessions/')) return Response.json(fx.SESSION);
        return new Response(fx.RESPONSE_BODY, {
          headers: {
            'x-aci-keyset-digest': responseDigests?.[index] ?? responseDigest,
            'x-receipt-id': fx.RECEIPT_PAYLOAD.receipt_id,
          },
        });
      },
      observedSpkiSha256: () => 'c0'.repeat(32),
      close: async () => {
        closed[index] = true;
      },
    };
    transports.push(transport);
    closed.push(false);
    return transport;
  };
  return { factory, transports, closed };
}

test('header renewal re-verifies once, keeps transport, and audits the receipt under its digest', async () => {
  const renewedKeyset = structuredClone(fx.KEYSET);
  renewedKeyset.not_after += 86_400;
  const renewedDigest = await computeKeysetDigest(renewedKeyset);
  const renewedReport = structuredClone(fx.REPORT);
  renewedReport.attestation.workload_keyset = renewedKeyset;
  renewedReport.workload_keyset_digest = renewedDigest;
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const renewed = identity(renewedReport, renewedKeyset, renewedDigest);
  const receiptPayload: Record<string, unknown> = {
    ...fx.RECEIPT_PAYLOAD, workload_keyset_digest: renewedDigest,
  };
  const receipt = await fx.makeDocument(receiptPayload);
  const { factory, transports, closed } = fakeTransportFactory(receipt, renewedDigest);
  let refreshes = 0;
  const connection = await connectAciWithTransport(
    { baseURL: 'https://api.example.com/v1', serving: { requireVerified: false } },
    factory,
    async () => (refreshes++ === 0 ? initial : renewed),
  );
  const request = {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: fx.REQUEST_BODY,
  };
  await (await connection.fetch('https://api.example.com/v1/chat/completions', request)).text();
  const audit = await connection.verifyReceipt(fx.RECEIPT_PAYLOAD.receipt_id);
  await (await connection.fetch('https://api.example.com/v1/chat/completions', request)).text();

  assert.equal(refreshes, 2);
  assert.equal(transports.length, 2);
  assert.equal(closed[0], false);
  assert.equal(closed[1], true);
  assert.equal(connection.identity.workloadKeysetDigest, renewedDigest);
  assert.equal(audit.transcript.lines.find((line) => line.id === 'receipt-1')?.status, 'pass');
  assert.equal(audit.transcript.lines.find((line) => line.id === 'receipt-2')?.status, 'pass');
});

test('a real rotation closes the old transport while its receipt uses the old identity', async () => {
  const rotatedKeyset = structuredClone(fx.KEYSET);
  rotatedKeyset.receipt_signing_keys = rotatedKeyset.receipt_signing_keys.map((entry, index) =>
    index === 0 ? { ...entry, public_key: '00'.repeat(32) } : entry,
  );
  const rotatedDigest = await computeKeysetDigest(rotatedKeyset);
  const rotatedReport = structuredClone(fx.REPORT);
  rotatedReport.attestation.workload_keyset = rotatedKeyset;
  rotatedReport.workload_keyset_digest = rotatedDigest;
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const rotated = identity(rotatedReport, rotatedKeyset, rotatedDigest);
  const { factory, transports, closed } = fakeTransportFactory(
    fx.ENVELOPE,
    rotatedDigest,
    [rotatedDigest, rotatedDigest],
  );
  let refreshes = 0;
  const connection = await connectAciWithTransport(
    { baseURL: 'https://api.example.com/v1', serving: { requireVerified: false } },
    factory,
    async () => (refreshes++ === 0 ? initial : rotated),
  );
  await (await connection.fetch('https://api.example.com/v1/chat/completions', {
    method: 'POST',
    body: fx.REQUEST_BODY,
  })).text();
  await assert.rejects(
    connection.fetch('https://api.example.com/v1/chat/completions', {
      method: 'POST',
      body: fx.REQUEST_BODY,
    }),
    (error: unknown) => error instanceof AciConnectionError && error.code === 'identity_expired',
  );

  assert.equal(refreshes, 2);
  assert.equal(transports.length, 2);
  assert.equal(closed[0], true);
  const audit = await connection.verifyReceipt(fx.RECEIPT_PAYLOAD.receipt_id);
  assert.equal(audit.transcript.lines.find((line) => line.id === 'receipt-1')?.status, 'pass');
  assert.equal(audit.transcript.lines.find((line) => line.id === 'receipt-2')?.status, 'pass');
});

test('an unknown receipt digest refreshes once and then fails closed', async () => {
  const unknownPayload = { ...fx.RECEIPT_PAYLOAD, workload_keyset_digest: 'unknown' };
  const unknownReceipt = await fx.makeDocument(unknownPayload as Record<string, unknown>);
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const { factory } = fakeTransportFactory(unknownReceipt, fx.KEYSET_DIGEST);
  let refreshes = 0;
  const connection = await connectAciWithTransport(
    { baseURL: 'https://api.example.com/v1', serving: { requireVerified: false } },
    factory,
    async () => (refreshes++ === 0 ? initial : initial),
  );
  await (await connection.fetch('https://api.example.com/v1/chat/completions', {
    method: 'POST',
    body: fx.REQUEST_BODY,
  })).text();
  await assert.rejects(
    connection.verifyReceipt(fx.RECEIPT_PAYLOAD.receipt_id),
    (error: unknown) => error instanceof AciConnectionError && error.code === 'receipt_verification',
  );
  assert.equal(refreshes, 2);
  await (await connection.fetch('https://api.example.com/v1/models')).text();
  assert.equal(refreshes, 2, 'a failed receipt audit must not block later sends');
});

test('replica B hints do not pin admission to B when fresh attestations keep landing on A', async () => {
  const replicaB = structuredClone(fx.KEYSET);
  replicaB.not_after += 86_400;
  const digestB = await computeKeysetDigest(replicaB);
  const receipt = await fx.makeDocument({ ...fx.RECEIPT_PAYLOAD, workload_keyset_digest: digestB });
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const { factory, closed } = fakeTransportFactory(receipt, digestB);
  let refreshes = 0;
  const connection = await connectAciWithTransport(
    { baseURL: 'https://api.example.com/v1', serving: { requireVerified: false } },
    factory,
    async () => { refreshes++; return initial; },
  );
  try {
    for (let i = 0; i < 3; i++) {
      await (await connection.fetch('https://api.example.com/v1/chat/completions', {
        method: 'POST', body: fx.REQUEST_BODY,
      })).text();
    }
    assert.equal(connection.receipts().length, 3);
    assert.equal(refreshes, 3);
    await assert.rejects(connection.verifyReceipt(), (error: unknown) =>
      error instanceof AciConnectionError && error.code === 'receipt_verification');
    await (await connection.fetch('https://api.example.com/v1/models')).text();
    assert.equal(connection.identity.workloadKeysetDigest, fx.KEYSET_DIGEST);
    assert.equal(closed[0], false);
  } finally {
    await connection.close();
  }
});

test('a delayed response naming a retired digest cannot strand admission', async (context) => {
  context.mock.timers.enable({ apis: ['Date'], now: (fx.NOT_AFTER - 1) * 1000 });
  const keyset = { ...fx.KEYSET, not_after: fx.NOT_AFTER + 86_400 };
  const digest = await computeKeysetDigest(keyset);
  const report = structuredClone(fx.REPORT);
  report.attestation.workload_keyset = keyset;
  report.workload_keyset_digest = digest;
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const renewed = identity(report, keyset, digest);
  const { factory: baseFactory } = fakeTransportFactory(fx.ENVELOPE, fx.KEYSET_DIGEST);
  let release: () => void = () => {};
  let reached: () => void = () => {};
  const resumed = new Promise<void>((resolve) => { release = resolve; });
  const started = new Promise<void>((resolve) => { reached = resolve; });
  let delayed = false;
  const factory: PinnedTransportFactory = (options) => {
    const transport = baseFactory(options);
    const fetch = transport.fetch;
    return {
      ...transport,
      async fetch(input, init) {
        if (!delayed && input instanceof Request && input.url.endsWith('/models')) {
          delayed = true;
          reached();
          await resumed;
        }
        return fetch(input, init);
      },
    };
  };
  let refreshes = 0;
  const connection = await connectAciWithTransport(
    { baseURL: 'https://api.example.com/v1' }, factory,
    async () => (refreshes++ === 0 ? initial : renewed),
  );
  try {
    const delayedResponse = connection.fetch('https://api.example.com/v1/models');
    await started;
    context.mock.timers.setTime(fx.NOT_AFTER * 1000);
    await connection.refresh();
    release();
    await (await delayedResponse).text();
    await (await connection.fetch('https://api.example.com/v1/models')).text();
    assert.equal(refreshes, 3);
  } finally {
    release();
    await connection.close();
  }
});

test('an exchange retains its established member for a historical receipt after expiry', async (context) => {
  context.mock.timers.enable({ apis: ['Date'], now: (fx.NOT_AFTER - 1) * 1000 });
  const keyset = { ...fx.KEYSET, not_after: fx.NOT_AFTER + 86_400 };
  const digest = await computeKeysetDigest(keyset);
  const report = structuredClone(fx.REPORT);
  report.attestation.workload_keyset = keyset;
  report.workload_keyset_digest = digest;
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const renewed = identity(report, keyset, digest);
  const { factory } = fakeTransportFactory(fx.ENVELOPE, fx.KEYSET_DIGEST);
  let refreshes = 0;
  const connection = await connectAciWithTransport(
    { baseURL: 'https://api.example.com/v1', serving: { requireVerified: false } }, factory,
    async () => (refreshes++ === 0 ? initial : renewed),
  );
  try {
    await (await connection.fetch('https://api.example.com/v1/chat/completions', {
      method: 'POST', body: fx.REQUEST_BODY,
    })).text();
    context.mock.timers.setTime(fx.NOT_AFTER * 1000);
    const audit = await connection.verifyReceipt();
    assert.equal(audit.transcript.lines.find((line) => line.id === 'receipt-2')?.status, 'pass');
  } finally {
    await connection.close();
  }
});

for (const change of ['subject', 'provenance', 'e2ee', 'tee', 'measurement']) {
  test(`a ${change} change closes the old transport and refuses the pending send`, async () => {
    const keyset = structuredClone(fx.KEYSET);
    const report = structuredClone(fx.REPORT);
    if (change === 'subject') keyset.subject = 'dstack-app://different-app';
    if (change === 'e2ee') keyset.e2ee_public_keys[0] = { key_id: 'other', algo: 'other', public_key: '00'.repeat(32) };
    if (change === 'provenance') report.attestation.source_provenance = { repo_commit: 'different' };
    if (change === 'tee') report.attestation.tee_type = 'different';
    const digest = await computeKeysetDigest(keyset);
    report.attestation.workload_keyset = keyset;
    report.workload_keyset_digest = digest;
    const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
    const changed = identity(report, keyset, digest);
    if (change === 'measurement') changed.composeHash = 'different';
    const { factory, closed } = fakeTransportFactory(fx.ENVELOPE, 'unseen');
    let refreshes = 0;
    const connection = await connectAciWithTransport(
      { baseURL: 'https://api.example.com/v1', serving: { requireVerified: false } }, factory,
      async () => (refreshes++ === 0 ? initial : changed),
    );
    try {
      await (await connection.fetch('https://api.example.com/v1/chat/completions', {
        method: 'POST', body: fx.REQUEST_BODY,
      })).text();
      await assert.rejects(connection.fetch('https://api.example.com/v1/models'), (error: unknown) =>
        error instanceof AciConnectionError && error.code === 'identity_expired');
      assert.equal(closed[0], true);
    } finally {
      await connection.close();
    }
  });
}

test('a receipt served at keyset expiry is rejected as expired', async () => {
  const expiredReceipt = await fx.makeDocument({
    ...fx.RECEIPT_PAYLOAD,
    served_at: fx.KEYSET.not_after,
  });
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const { factory } = fakeTransportFactory(expiredReceipt, fx.KEYSET_DIGEST);
  const connection = await connectAciWithTransport(
    { baseURL: 'https://api.example.com/v1', serving: { requireVerified: false } },
    factory,
    async () => initial,
  );

  await (await connection.fetch('https://api.example.com/v1/chat/completions', {
    method: 'POST',
    body: fx.REQUEST_BODY,
  })).text();
  const audit = await connection.verifyReceipt(fx.RECEIPT_PAYLOAD.receipt_id);
  const check = audit.transcript.lines.find((line) => line.id === 'receipt-2');
  assert.equal(audit.transcript.lines.find((line) => line.id === 'receipt-1')?.status, 'pass');
  assert.equal(check?.status, 'fail');
  assert.match(check?.detail ?? '', /served_at.*before not_after/);
});

test('an unestablished digest hint reports re-verification failure without claiming expiry', async () => {
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const { factory } = fakeTransportFactory(fx.ENVELOPE, 'unseen');
  let refreshes = 0;
  const connection = await connectAciWithTransport(
    { baseURL: 'https://api.example.com/v1' }, factory,
    async () => {
      if (refreshes++ === 0) return initial;
      throw new AciConnectionError('attestation_verification', 'quote verification failed');
    },
  );
  try {
    await (await connection.fetch('https://api.example.com/v1/chat/completions', {
      method: 'POST', headers: { 'content-type': 'application/json' }, body: fx.REQUEST_BODY,
    })).text();
    await assert.rejects(connection.fetch('https://api.example.com/v1/models'), (error: unknown) =>
      error instanceof AciConnectionError && error.code === 'attestation_verification'
        && /unestablished digest/.test(error.message));
    assert.equal(connection.receipts().length, 1);
  } finally {
    await connection.close();
  }
});

test('invalid bodies and multipart carriers cause zero inference sends', async () => {
  let sends = 0;
  const initial = identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST);
  const factory: PinnedTransportFactory = () => ({
    fetch: async (input) => {
      if (new URL(input instanceof Request ? input.url : input.toString()).pathname.endsWith('/attestation')) {
        return Response.json({ service_capabilities: { supported_multipart_constraint_endpoints: [audioPath] } });
      }
      sends++;
      return new Response(null, { status: 204 });
    },
    observedSpkiSha256: () => 'c0'.repeat(32),
    close: async () => {},
  });
  const connection = await connectAciWithTransport({
    baseURL: 'https://api.example.com/v1',
    serving: { acceptedSessionIds: ['a'.repeat(64)] },
  }, factory, async () => initial);
  const request = (carrier?: string): RequestInit => ({
    method: 'POST',
    headers: { 'content-type': multipartType, ...(carrier === undefined ? {} : { 'x-aci-provider': carrier }) },
    body: Buffer.from(multipartBody()),
  });
  try {
    const cases: [string, RequestInit][] = [
      ...['not json', '[]', 'null', '1', '{'].map((body): [string, RequestInit] => [
        audioPath, { method: 'POST', headers: { 'content-type': 'application/json' }, body },
      ]),
      [audioPath, { method: 'POST', headers: { 'content-type': 'text/plain' }, body: '{}' }],
      [audioPath, { method: 'POST', headers: { 'content-type': 'application/octet-stream' }, body: Buffer.from([0, 255]) }],
      ['/v1/chat/completions', request()],
      ...['{', 'null', '[]', '{} , {}', '{"order":["café"]}', '{"aci_future":true}', '{"aci_verified":"yes"}',
        '{"aci_session_ids":[]}', '{"aci_session_ids":["bad"]}',
        JSON.stringify({ aci_session_ids: ['b'.repeat(64)] }),
        JSON.stringify({ aci_verified: false, aci_session_ids: ['a'.repeat(64)] }),
        JSON.stringify({ padding: 'a'.repeat(8192) }),
      ].map((carrier): [string, RequestInit] => [audioPath, request(carrier)]),
      [audioPath, { ...request(), body: Buffer.from(multipartBody('provider')) }],
      [audioPath, { ...request(), body: Buffer.concat([
        Buffer.from(multipartBody()).subarray(0, -Buffer.byteLength('--audio-test--\r\n')),
        Buffer.from('--audio-test\r\nContent-Disposition: form-data; name="provider"\r\n\r\n{}\r\n--audio-test--\r\n'),
      ]) }],
      [audioPath, { ...request(), body: 'broken multipart' }],
      [audioPath, { ...request(), body: '--audio-test--\r\n' }],
      [audioPath, { ...request(), body: '--audio-test\r\nContent-Disposition: form-data; name="file"; filename="audio.wav"\r\n\r\ndata\r\n--audio-test--\r\n' }],
      // Bun can parse LF-only framing as empty FormData, hiding the provider part.
      [audioPath, { ...request(), body: '--audio-test\nContent-Disposition: form-data; name="model"\n\nm\n--audio-test\nContent-Disposition: form-data; name="provider"\n\n{}\n--audio-test--\n' }],
      [audioPath, { ...request(), headers: [['content-type', multipartType], ['x-aci-provider', '{}'], ['x-aci-provider', '{}']] }],
      // These fragments form valid JSON after Fetch joins them with ", ".
      [audioPath, { ...request(), headers: [['content-type', multipartType], ['X-ACI-Provider', '{"order":["a"'], ['x-aci-provider', '"b"]}']] }],
      [audioPath, { ...request(), headers: { 'content-type': multipartType, 'X-ACI-Provider': '{"order":["a"', 'x-aci-provider': '"b"]}' } }],
      [audioPath, { ...request(), headers: new Headers([['content-type', multipartType], ['X-ACI-Provider', '{}'], ['x-aci-provider', '{}']]) }],
      [audioPath, { ...request(), headers: { 'content-type': multipartType, connection: 'x-aci-provider' } }],
      ['/v1/chat/completions', { method: 'POST', headers: { 'content-type': 'application/json', 'x-aci-provider': '{}' }, body: '{}' }],
    ];
    for (const [path, init] of cases) {
      await assert.rejects(connection.fetch(`https://api.example.com${path}`, init), (error: unknown) =>
        error instanceof AciConnectionError && error.code === 'invalid_serving_constraints');
      assert.equal(sends, 0, `no upload allowed for ${path}`);
    }
  } finally {
    await connection.close();
  }
});

test('multipart support must be learned over the pinned channel before any upload', async () => {
  let sends = 0;
  const bootstrap = structuredClone(fx.REPORT);
  bootstrap.service_capabilities = { supported_multipart_constraint_endpoints: [audioPath] };
  const { factory: baseFactory } = fakeTransportFactory(fx.ENVELOPE, fx.KEYSET_DIGEST);
  const factory: PinnedTransportFactory = (options) => {
    const transport = baseFactory(options);
    return { ...transport, fetch: async (input, init) => {
      if (input instanceof Request && input.method === 'POST') sends++;
      return transport.fetch(input, init);
    } };
  };
  const connection = await connectAciWithTransport({ baseURL: 'https://api.example.com/v1' }, factory,
    async () => identity(bootstrap, fx.KEYSET, fx.KEYSET_DIGEST));
  try {
    await assert.rejects(connection.fetch(`https://api.example.com${audioPath}`, {
      method: 'POST', headers: { 'content-type': multipartType }, body: Buffer.from(multipartBody()),
    }), /does not advertise multipart/);
    assert.equal(sends, 0);
  } finally {
    await connection.close();
  }
});

test('multipart preserves wire bytes and audits the effective header pins with local enforcement on or off', async () => {
  const body = multipartBody();
  const bodyHash = await hashBody(body);
  const receipt = await fx.makeDocument({
    ...fx.RECEIPT_PAYLOAD,
    event_log: fx.RECEIPT_PAYLOAD.event_log.map((event) =>
      event.type === 'request.received' || event.type === 'request.forwarded' ? { ...event, body_hash: bodyHash } : event),
  });
  for (const requireVerified of [true, false]) {
    for (const accepted of [fx.SESSION_ID, 'b'.repeat(64)]) {
      let sends = 0;
      const { factory: baseFactory } = fakeTransportFactory(receipt, fx.KEYSET_DIGEST);
      const factory: PinnedTransportFactory = (options) => {
        const transport = baseFactory(options);
        return { ...transport, fetch: async (input, init) => {
          if (new URL(input instanceof Request ? input.url : input.toString()).pathname.endsWith('/attestation')) {
            return Response.json({ service_capabilities: { supported_multipart_constraint_endpoints: [audioPath] } });
          }
          if (input instanceof Request && input.method === 'POST') {
            sends++;
            assert.deepEqual(new Uint8Array(await input.clone().arrayBuffer()), new Uint8Array(body));
            const carrier = input.headers.get('x-aci-provider') ?? '';
            assert.doesNotMatch(carrier, /[\u0080-\uffff]/);
            assert.deepEqual(JSON.parse(carrier), {
              order: ['upstream, primary', 'café 🎙'], aci_session_ids: [accepted], aci_verified: true,
            });
          }
          return transport.fetch(input, init);
        } };
      };
      const connection = await connectAciWithTransport({
        baseURL: 'https://api.example.com/v1',
        serving: requireVerified ? { acceptedSessionIds: [accepted] } : { requireVerified: false },
      }, factory, async () => identity(fx.REPORT, fx.KEYSET, fx.KEYSET_DIGEST));
      try {
        await (await connection.fetch(`https://api.example.com${audioPath}`, {
          method: 'POST',
          headers: {
            'content-type': multipartType,
            'x-aci-provider': JSON.stringify({ order: ['upstream, primary', 'café 🎙'], aci_session_ids: requireVerified ? [accepted, 'c'.repeat(64)] : [accepted] }).replace('café 🎙', String.raw`caf\u00e9 \ud83c\udf99`),
          },
          body: Buffer.from(body),
        })).text();
        const audit = await connection.verifyReceipt();
        assert.equal(sends, 1);
        assert.equal(audit.transcript.lines.find((line) => line.id === 'receipt-3')?.status, 'pass');
        assert.equal(audit.transcript.lines.find((line) => line.id === 'upstream-2')?.status,
          accepted === fx.SESSION_ID ? 'pass' : 'fail');
      } finally {
        await connection.close();
      }
    }
  }
});
