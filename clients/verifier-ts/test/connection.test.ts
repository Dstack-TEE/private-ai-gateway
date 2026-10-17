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
  const receiptPayload: Record<string, unknown> = { ...fx.RECEIPT_PAYLOAD };
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
  await (await connection.fetch('https://api.example.com/v1/chat/completions', request)).text();

  assert.equal(refreshes, 2);
  assert.equal(transports.length, 2);
  assert.equal(closed[0], false);
  assert.equal(closed[1], true);
  assert.equal(connection.identity.digests.length, 2);
  assert.equal(connection.identity.workloadKeysetDigest, renewedDigest);
  const audit = await connection.verifyReceipt(fx.RECEIPT_PAYLOAD.receipt_id);
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
    (error: unknown) => error instanceof AciConnectionError && error.code === 'identity_expired',
  );
  assert.equal(refreshes, 2);
});

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
