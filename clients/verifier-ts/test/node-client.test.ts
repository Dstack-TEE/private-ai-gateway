import assert from 'node:assert/strict';
import { after, test } from 'node:test';

import { install } from 'undici';

import { computeKeysetDigest, computeReportData } from '../src/index.js';
import { constrainJsonBody } from '../src/runtime/connection.js';
import { connectAci, AciConnectionError } from '../src/node/index.js';
import { createPinnedTransport } from '../src/node/transport.js';
import type { AttestationReport, WorkloadKeyset } from '../src/types.js';
import {
  assertPinnedTransport,
  cleanupPinnedTransportFixture,
} from './pinned-transport-fixture.js';

install();

test('connectAci rejects a non-HTTPS base URL before making a request', async () => {
  await assert.rejects(
    connectAci({ baseURL: 'http://gateway.example/v1' }),
    (error: unknown) =>
      error instanceof AciConnectionError && error.code === 'invalid_base_url',
  );
});

test('connectAci rejects contradictory or malformed serving policy before network access', async () => {
  for (const serving of [
    { acceptedSessionIds: [] },
    { acceptedSessionIds: ['AB'.repeat(32)] },
    { requireVerified: false, acceptedSessionIds: ['ab'.repeat(32)] },
  ]) {
    await assert.rejects(
      connectAci({ baseURL: 'https://gateway.example/v1', serving }),
      (error: unknown) =>
        error instanceof AciConnectionError && error.code === 'invalid_policy',
    );
  }
});

test('serving policy composes request pins with the locally accepted set', () => {
  const accepted = ['aa'.repeat(32), 'bb'.repeat(32)];
  const encode = (value: object) => new TextEncoder().encode(JSON.stringify(value));
  const decode = (value: Uint8Array) => JSON.parse(new TextDecoder().decode(value));

  const constrained = constrainJsonBody(
    encode({ provider: { aci_session_ids: [accepted[1], 'cc'.repeat(32)] } }),
    { baseURL: 'https://gateway.example/v1', serving: { acceptedSessionIds: accepted } },
  );
  assert.deepEqual(decode(constrained.body).provider, {
    aci_session_ids: [accepted[1]],
    aci_verified: true,
  });
  assert.deepEqual(constrained.pinnedSessions, [accepted[1]]);

  const callerPins = constrainJsonBody(
    encode({ provider: { aci_session_ids: [accepted[0]] } }),
    { baseURL: 'https://gateway.example/v1', serving: { requireVerified: false } },
  );
  assert.deepEqual(callerPins.pinnedSessions, [accepted[0]]);
  assert.equal(decode(callerPins.body).provider.aci_verified, true);

  assert.throws(
    () =>
      constrainJsonBody(encode({ provider: { aci_session_ids: ['cc'.repeat(32)] } }), {
        baseURL: 'https://gateway.example/v1',
        serving: { acceptedSessionIds: accepted },
      }),
    (error: unknown) =>
      error instanceof AciConnectionError && error.code === 'invalid_serving_constraints',
  );
});

test('connectAci rejects a self-consistent report without a hardware quote', async () => {
  const nonceReport = async (url: string): Promise<AttestationReport> => {
    const nonce = new URL(url).searchParams.get('nonce');
    assert.ok(nonce);
    const keyset: WorkloadKeyset = {
      not_after: Math.floor(Date.now() / 1000) + 300,
      receipt_signing_keys: [],
      e2ee_public_keys: [],
      tls_public_keys: [{ domain: 'gateway.example', spki_sha256: '11'.repeat(32) }],
    };
    const digest = await computeKeysetDigest(keyset);
    return {
      api_version: 'aci/1',
      workload_keyset_digest: digest,
      attestation: {
        tee_type: 'tdx',
        workload_keyset: keyset,
        report_data: await computeReportData(digest, nonce),
      },
    };
  };

  await assert.rejects(
    connectAci({
      baseURL: 'https://gateway.example/v1',
      bootstrapFetch: async (input) =>
        Response.json(await nonceReport(typeof input === 'string' ? input : input.toString())),
    }),
    (error: unknown) =>
      error instanceof AciConnectionError &&
      error.code === 'attestation_verification' &&
      error.message.includes('id-1') &&
      error.message.includes('id-4'),
  );
});

after(cleanupPinnedTransportFixture);

test('Node fetch enforces the shared pinned transport contract', () =>
  assertPinnedTransport(createPinnedTransport));
