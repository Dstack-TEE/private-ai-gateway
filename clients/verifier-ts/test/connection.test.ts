import assert from 'node:assert/strict';
import { test } from 'node:test';

import { computeKeysetDigest } from '../src/index.js';
import { connectAciWithTransport } from '../src/runtime/connection.js';
import { AciConnectionError, type EstablishedDigest, type VerifiedAciIdentity } from '../src/runtime/types.js';
import type { PinnedTransport, PinnedTransportFactory } from '../src/runtime/transport.js';
import type { AttestationReport, ReceiptEnvelope, WorkloadKeyset } from '../src/types.js';
import * as fx from './fixtures.js';

function identity(report: AttestationReport, keyset: WorkloadKeyset, digest: string): VerifiedAciIdentity {
  const established: EstablishedDigest = {
    digest,
    notAfter: keyset.not_after * 1000,
    keyset,
    report,
  };
  return {
    origin: 'https://api.example.com',
    hostname: 'api.example.com',
    report,
    keyset,
    workloadKeysetDigest: digest,
    composeHash: 'compose',
    tlsSpkiPins: ['c0'.repeat(32)],
    verifiedAt: 1,
    expiresAt: established.notAfter,
    digests: [established],
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
  assert.equal(connection.identity.digests.length, 2);
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
    assert.equal(connection.identity.digests.some((entry) => entry.digest === digestB), false);
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
    assert.equal(connection.identity.digests.some((entry) => entry.digest === fx.KEYSET_DIGEST), false);
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
    assert.equal(connection.identity.digests.some((entry) => entry.digest === fx.KEYSET_DIGEST), false);
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
      assert.equal(connection.identity.digests.length, 1);
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
  await assert.rejects(
    connection.verifyReceipt(fx.RECEIPT_PAYLOAD.receipt_id),
    (error: unknown) => error instanceof AciConnectionError && error.code === 'receipt_verification',
  );
});
