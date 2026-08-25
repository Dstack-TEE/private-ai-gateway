import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { X509Certificate, createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import https from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { after, test } from 'node:test';

import { install } from 'undici';

import { computeKeysetDigest, computeReportData } from '../src/index.js';
import { connectAci, AciConnectionError } from '../src/node/index.js';
import { createPinnedTransport } from '../src/node/transport.js';
import type { AttestationReport, WorkloadKeyset } from '../src/types.js';

install();

test('connectAci rejects a non-HTTPS base URL before making a request', async () => {
  await assert.rejects(
    connectAci({ baseURL: 'http://gateway.example/v1' }),
    (error: unknown) =>
      error instanceof AciConnectionError && error.code === 'invalid_base_url',
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
      policy: { requireComposeMeasurement: false },
      bootstrapFetch: async (input) =>
        Response.json(await nonceReport(typeof input === 'string' ? input : input.toString())),
    }),
    (error: unknown) =>
      error instanceof AciConnectionError &&
      error.code === 'attestation_verification' &&
      error.message.includes('id-1'),
  );
});

const certDir = mkdtempSync(join(tmpdir(), 'aci-client-pin-'));

function openssl(args: string[]): void {
  execFileSync('openssl', args, { cwd: certDir, stdio: 'ignore' });
}

openssl([
  'req',
  '-x509',
  '-newkey',
  'rsa:2048',
  '-nodes',
  '-keyout',
  'ca.key',
  '-out',
  'ca.crt',
  '-days',
  '2',
  '-subj',
  '/CN=aci-client-test-ca',
]);
writeFileSync(join(certDir, 'ext.cnf'), 'subjectAltName=DNS:localhost,IP:127.0.0.1\n');
openssl([
  'req',
  '-newkey',
  'rsa:2048',
  '-nodes',
  '-keyout',
  'leaf.key',
  '-out',
  'leaf.csr',
  '-subj',
  '/CN=localhost',
  '-addext',
  'subjectAltName=DNS:localhost,IP:127.0.0.1',
]);
openssl([
  'x509',
  '-req',
  '-in',
  'leaf.csr',
  '-CA',
  'ca.crt',
  '-CAkey',
  'ca.key',
  '-CAcreateserial',
  '-out',
  'leaf.crt',
  '-days',
  '2',
  '-extfile',
  'ext.cnf',
]);

const ca = readFileSync(join(certDir, 'ca.crt'), 'utf8');
const cert = readFileSync(join(certDir, 'leaf.crt'), 'utf8');
const key = readFileSync(join(certDir, 'leaf.key'), 'utf8');
const spki = createHash('sha256')
  .update(new X509Certificate(cert).publicKey.export({ type: 'spki', format: 'der' }))
  .digest('hex');

async function startServer(body: string): Promise<{
  origin: string;
  url: string;
  close(): Promise<void>;
}> {
  const server = https.createServer({ cert, key }, (_request, response) => {
    response.writeHead(200, { 'content-type': 'text/plain' });
    response.end(body);
  });
  server.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  const address = server.address();
  assert.ok(address && typeof address === 'object');
  const origin = `https://127.0.0.1:${address.port}`;
  return {
    origin,
    url: `${origin}/v1/models`,
    close: () => new Promise<void>((resolve) => server.close(() => resolve())),
  };
}

after(() => {
  rmSync(certDir, { recursive: true, force: true });
});

test('pinned transport enforces the SPKI and exact origin without global state', async () => {
  const primary = await startServer('primary');
  const other = await startServer('other');
  const transport = createPinnedTransport({
    origin: primary.origin,
    hostname: '127.0.0.1',
    spkiSha256: spki,
    ca,
  });
  try {
    assert.equal(await transport.fetch(primary.url).then((response) => response.text()), 'primary');
    await assert.rejects(
      transport.fetch(other.url),
      (error: unknown) =>
        error instanceof AciConnectionError && error.code === 'origin_mismatch',
    );
  } finally {
    await transport.close();
    await primary.close();
    await other.close();
  }

  const mismatchServer = await startServer('mismatch');
  const mismatched = createPinnedTransport({
    origin: mismatchServer.origin,
    hostname: '127.0.0.1',
    spkiSha256: '00'.repeat(32),
    ca,
  });
  try {
    await assert.rejects(mismatched.fetch(mismatchServer.url), /SPKI pin mismatch/);
  } finally {
    await mismatched.close();
    await mismatchServer.close();
  }
});
