import assert from "node:assert/strict";
import { test, after } from "node:test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import https from "node:https";
import crypto from "node:crypto";
import undici from "undici";

import {
  clearPins,
  installFetchPinning,
  isFetchPinningInstalled,
  setPin,
  setPinningCaForTests,
} from "../src/tls-pinning.ts";

// Mimic pi's runtime: the global fetch becomes undici's fetch (the only one
// that honors the per-request `dispatcher` option). A local CA signs the test
// server certs, so the TLS chain validates and Node performs the hostname/
// identity check that the pin rides on. The baseline global dispatcher trusts
// the same local CA, so unpinned hosts connect normally and only the pin's
// checkServerIdentity is the discriminator.
undici.install?.();

const dir = mkdtempSync(join(tmpdir(), "phala-pin-"));

function run(args: string[]) {
  execFileSync("openssl", args, { cwd: dir, stdio: "ignore" });
}

run(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", "ca.key", "-out", "ca.crt",
  "-days", "2", "-subj", "/CN=phala-test-ca"]);
const caPem = readFileSync(join(dir, "ca.crt"), "utf8");
writeFileSync(join(dir, "ext.cnf"), "subjectAltName=DNS:localhost,IP:127.0.0.1\n");

function makeLeaf(name: string): { key: string; cert: string; spki: string } {
  run(["req", "-newkey", "rsa:2048", "-nodes", "-keyout", `${name}.key`, "-out", `${name}.csr`,
    "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1"]);
  run(["x509", "-req", "-in", `${name}.csr`, "-CA", "ca.crt", "-CAkey", "ca.key", "-CAcreateserial",
    "-out", `${name}.crt`, "-days", "2", "-extfile", "ext.cnf"]);
  const key = readFileSync(join(dir, `${name}.key`), "utf8");
  const cert = readFileSync(join(dir, `${name}.crt`), "utf8");
  const x509 = new crypto.X509Certificate(cert);
  const spki = crypto.createHash("sha256")
    .update(x509.publicKey.export({ type: "spki", format: "der" }) as Buffer)
    .digest("hex");
  return { key, cert, spki };
}

const leafA = makeLeaf("a");

// Baseline dispatcher trusts the local CA so unpinned connections validate.
undici.setGlobalDispatcher(
  new undici.EnvHttpProxyAgent({ allowH2: false, connect: { rejectUnauthorized: true, ca: caPem } }),
);
setPinningCaForTests(caPem);

// A fresh server per scenario: undici pools connections by origin, so a bare
// `127.0.0.1` reuse across asserts would skip re-validation (correct behavior
// for the same attested peer, but it defeats these tests). A distinct port
// gives each scenario its own connection pool.
async function startServer(cert: { key: string; cert: string }, id: string) {
  const srv = https.createServer({ key: cert.key, cert: cert.cert }, (_req, res) => {
    res.writeHead(200, { "content-type": "text/plain" });
    res.end(id);
  });
  srv.listen(0, "127.0.0.1");
  await new Promise<void>((resolve) => srv.once("listening", resolve));
  const port = (srv.address() as { port: number }).port;
  return {
    url: `https://127.0.0.1:${port}/`,
    close: () => new Promise<void>((r) => srv.close(() => r())),
  };
}

after(() => {
  rmSync(dir, { recursive: true, force: true });
});

test("fetch wrapper: delegates unpinned hosts; pins only the pinned host", async () => {
  const pinned = await startServer(leafA, "pinned-ok");
  const unpinned = await startServer(leafA, "plain");
  try {
    assert.equal(isFetchPinningInstalled(), false);
    installFetchPinning();
    assert.equal(isFetchPinningInstalled(), true);

    // Unrelated host, no pin: delegated unchanged (baseline dispatcher).
    clearPins();
    assert.equal(await globalThis.fetch(unpinned.url).then((r) => r.text()), "plain");

    // Pin the host to the server's attested SPKI: connection succeeds.
    setPin("127.0.0.1", leafA.spki);
    assert.equal(await globalThis.fetch(pinned.url).then((r) => r.text()), "pinned-ok");

    // Non-pinned host still unaffected.
    assert.equal(await globalThis.fetch(unpinned.url).then((r) => r.text()), "plain");
  } finally {
    clearPins();
    await pinned.close();
    await unpinned.close();
  }
});

test("pinning fails closed on SPKI mismatch", async () => {
  const srv = await startServer(leafA, "nope");
  try {
    // Wrong pin for this host: the TLS handshake must be rejected.
    setPin("127.0.0.1", "00".repeat(32));
    await assert.rejects(globalThis.fetch(srv.url));
  } finally {
    clearPins();
    await srv.close();
  }
});

test("changing the pin applies on the next request (key rotation)", async () => {
  const srv = await startServer(leafA, "rotated-ok");
  try {
    setPin("127.0.0.1", "11".repeat(32));
    await assert.rejects(globalThis.fetch(srv.url));

    // Rotate to the correct pin: recovers without reinstalling the wrapper.
    setPin("127.0.0.1", leafA.spki);
    assert.equal(await globalThis.fetch(srv.url).then((r) => r.text()), "rotated-ok");
  } finally {
    clearPins();
    await srv.close();
  }
});