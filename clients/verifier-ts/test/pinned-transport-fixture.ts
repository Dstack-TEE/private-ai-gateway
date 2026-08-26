import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { X509Certificate, createHash } from 'node:crypto';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import https from 'node:https';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { AciConnectionError } from '../src/runtime/types.js';
import type { PinnedTransportFactory } from '../src/runtime/transport.js';

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

export function cleanupPinnedTransportFixture(): void {
  rmSync(certDir, { recursive: true, force: true });
}

export async function assertPinnedTransport(factory: PinnedTransportFactory): Promise<void> {
  const primary = await startServer('primary');
  const other = await startServer('other');
  const transport = factory({
    origin: primary.origin,
    hostname: '127.0.0.1',
    spkiPins: ['00'.repeat(32), spki],
    ca,
  });
  try {
    assert.equal(await transport.fetch(primary.url).then((response) => response.text()), 'primary');
    await assert.rejects(
      transport.fetch(other.url),
      (error: unknown) =>
        error instanceof AciConnectionError && error.code === 'origin_mismatch',
    );
    await transport.close();
    await assert.rejects(
      transport.fetch(primary.url),
      (error: unknown) => error instanceof AciConnectionError && error.code === 'closed',
    );
  } finally {
    await transport.close();
    await primary.close();
    await other.close();
  }

  const mismatchServer = await startServer('mismatch');
  const mismatched = factory({
    origin: mismatchServer.origin,
    hostname: '127.0.0.1',
    spkiPins: ['00'.repeat(32)],
    ca,
  });
  try {
    await assert.rejects(mismatched.fetch(mismatchServer.url), /SPKI pin mismatch/);
  } finally {
    await mismatched.close();
    await mismatchServer.close();
  }
}
