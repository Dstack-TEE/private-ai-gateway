import assert from "node:assert/strict";
import { test } from "node:test";
import { createHash } from "node:crypto";

import {
  type CapturedExchange,
  TlsPinManager,
  computeSpkiSha256Hex,
  createAciFetch,
  hostOfInput,
  isInferencePath,
} from "../src/pinned-fetch.ts";

const GATEWAY = "gateway.example";
const PIN = "a".repeat(64);

function streamOf(chunks: Uint8Array[]): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(chunk);
      controller.close();
    },
  });
}

function fakeResponse(body?: ReadableStream<Uint8Array>, headers: Record<string, string> = {}) {
  return new Response(body ?? null, { status: 200, headers });
}

interface FakeFetchCall {
  url: string;
  init?: RequestInit;
}

function makeDeps(over: Partial<Parameters<typeof createAciFetch>[0]> = {}) {
  const manager = new TlsPinManager();
  const calls: FakeFetchCall[] = [];
  const exchanges: CapturedExchange[] = [];
  const baseFetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    calls.push({ url: String(input), init });
    return fakeResponse(streamOf([new TextEncoder().encode("data: hello\n\n")]), {
      "x-receipt-id": "rcpt-1",
    });
  }) as typeof fetch;
  const deps: Parameters<typeof createAciFetch>[0] = {
    manager,
    isGatewayHost: (host) => host === GATEWAY,
    pinningEnabled: () => true,
    failOpenOnUnpinned: () => false,
    ensurePinned: async () => false,
    onExchange: (x) => exchanges.push(x),
    baseFetch,
    ...over,
  };
  return { manager, calls, exchanges, deps };
}

test("non-gateway hosts pass through untouched (no tls init, no capture)", async () => {
  const { calls, exchanges, deps } = makeDeps();
  const fetcher = createAciFetch(deps);
  const res = await fetcher(`https://api.other.com/v1/chat`);
  await res.text();
  assert.equal(calls.length, 1);
  assert.equal((calls[0].init as Record<string, unknown> | undefined)?.tls, undefined);
  assert.equal(exchanges.length, 0);
});

test("fail closed: no pin + failOpen off blocks before any request leaves", async () => {
  const { calls, deps } = makeDeps();
  const fetcher = createAciFetch(deps);
  await assert.rejects(
    fetcher(`https://${GATEWAY}/v1/chat/completions`, { method: "POST" }),
    /requires an attested TLS pin/,
  );
  assert.equal(calls.length, 0);
});

test("lazy pin install: ensurePinned runs on first inference use and the pin is enforced via tls init", async () => {
  const { manager, calls, deps } = makeDeps({
    ensurePinned: async (host) => {
      manager.setPin(host, PIN);
      return true;
    },
  });
  const fetcher = createAciFetch(deps);
  const res = await fetcher(`https://${GATEWAY}/v1/chat/completions`, {
    method: "POST",
    body: "{}",
  });
  await res.text();
  assert.equal(calls.length, 1);
  const tls = (calls[0].init as Record<string, unknown>)?.tls as
    | { checkServerIdentity?: unknown; rejectUnauthorized?: boolean }
    | undefined;
  assert.equal(typeof tls?.checkServerIdentity, "function");
  assert.equal(tls?.rejectUnauthorized, true);
});

test("fail open: unpinnable + failOpenOnUnpinned proceeds without tls init", async () => {
  const { calls, deps } = makeDeps({
    failOpenOnUnpinned: () => true,
  });
  const fetcher = createAciFetch(deps);
  const res = await fetcher(`https://${GATEWAY}/v1/chat/completions`, { method: "POST" });
  await res.text();
  assert.equal(calls.length, 1);
  assert.equal((calls[0].init as Record<string, unknown>)?.tls, undefined);
});

test("pinning disabled: no ensurePinned, no tls init", async () => {
  let ensureCalls = 0;
  const { calls, deps } = makeDeps({
    pinningEnabled: () => false,
    ensurePinned: async () => {
      ensureCalls++;
      return false;
    },
  });
  const fetcher = createAciFetch(deps);
  const res = await fetcher(`https://${GATEWAY}/v1/chat/completions`, { method: "POST" });
  await res.text();
  assert.equal(ensureCalls, 0);
  assert.equal(calls.length, 1);
  assert.equal((calls[0].init as Record<string, unknown>)?.tls, undefined);
});

test("ACI bootstrap endpoints stay reachable without a pin", async () => {
  const { calls, deps } = makeDeps();
  const fetcher = createAciFetch(deps);
  const res = await fetcher(`https://${GATEWAY}/v1/aci/attestation?nonce=abc`);
  await res.text();
  assert.equal(calls.length, 1);
});

test("capture: request body, response bytes, and receipt headers reach onExchange", async () => {
  const { exchanges, deps } = makeDeps({
    ensurePinned: async () => true, // pin must exist for tls path; not required for capture
  });
  // No pin installed, failOpen off — but bootstrap paths bypass the gate; use
  // failOpen to let this inference request through unpinned.
  const openDeps = { ...deps, failOpenOnUnpinned: () => true };
  const fetcher = createAciFetch(openDeps);
  const payload = JSON.stringify({ model: "m", messages: [] });
  const res = await fetcher(`https://${GATEWAY}/v1/chat/completions`, {
    method: "POST",
    body: payload,
  });
  const text = await res.text();
  assert.equal(text, "data: hello\n\n");
  assert.equal(exchanges.length, 1);
  const x = exchanges[0];
  assert.equal(x.headers["x-receipt-id"], "rcpt-1");
  assert.equal(new TextDecoder().decode(x.requestBody), payload);
  assert.equal(new TextDecoder().decode(x.responseBytes), "data: hello\n\n");
});

test("cancelled streams never report an exchange (no partial-byte mismatch)", async () => {
  const { exchanges, deps } = makeDeps({ failOpenOnUnpinned: () => true });
  const hanging = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode("data: partial\n\n"));
      // never closes
    },
  });
  const baseFetch = (async () => fakeResponse(hanging)) as typeof fetch;
  const fetcher = createAciFetch({ ...deps, baseFetch });
  const res = await fetcher(`https://${GATEWAY}/v1/chat/completions`, { method: "POST" });
  const reader = res.body!.getReader();
  await reader.read();
  await reader.cancel();
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.equal(exchanges.length, 0);
});

test("checkServerIdentity: matching SPKI accepts, mismatch rejects, host:port normalized", () => {
  const manager = new TlsPinManager();
  const pubkey = new Uint8Array([1, 2, 3, 4, 5]);
  const spki = createHash("sha256").update(pubkey).digest("hex");
  manager.setPin(GATEWAY, spki);

  assert.equal(manager.checkServerIdentity(GATEWAY, { pubkey }), undefined);
  // TLS callbacks report host:port for non-default ports.
  assert.equal(manager.checkServerIdentity(`${GATEWAY}:8443`, { pubkey }), undefined);

  const wrong = manager.checkServerIdentity(GATEWAY, { pubkey: new Uint8Array([9, 9]) });
  assert.ok(wrong instanceof Error);
  assert.match(wrong.message, /pin mismatch/);
});

test("checkServerIdentity: no pin for host defers to default TLS validation", () => {
  const manager = new TlsPinManager();
  assert.equal(
    manager.checkServerIdentity("unpinned.example", { pubkey: new Uint8Array([1]) }),
    undefined,
  );
});

test("computeSpkiSha256Hex derives SPKI from cert raw DER (gateway-compatible)", () => {
  // Real leaf certificate once served by inference.phala.com. The gateway's
  // attested spki_sha256 for this cert is the openssl-verified value below;
  // hashing cert.pubkey instead (the old behavior) produced a different,
  // never-matching digest - the SPKI must come from the full cert DER.
  const CERT_DER_BASE64 =
    "MIIDlDCCAxqgAwIBAgISBb093fqYrLC2fDpObGU07PL7MAoGCCqGSM49BAMDMDMxCzAJBgNVBAYTAlVTMRYwFAYDVQQKEw1MZXQncyBFbmNyeXB0MQwwCgYDVQQDEwNZRTEwHhcNMjYwNjI0MDY0MTAwWhcNMjYwOTIyMDY0MDU5WjAeMRwwGgYDVQQDExNpbmZlcmVuY2UucGhhbGEuY29tMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEPfiFKSLz1+UeZuPvvNE681M/nHBvHkxZozrZkRNtgppzOCLFDoitIutMRPhrp8bdFLjk7Ncr4q8u/ZOWy+pi0qOCAiEwggIdMA4GA1UdDwEB/wQEAwIHgDATBgNVHSUEDDAKBggrBgEFBQcDATAMBgNVHRMBAf8EAjAAMB0GA1UdDgQWBBTto+LNWMGhCKvBfmJz0yi+VnpIkjAfBgNVHSMEGDAWgBS7IMpHC/7X5Zz5jwkqo4w3RbG82DAzBggrBgEFBQcBAQQnMCUwIwYIKwYBBQUHMAKGF2h0dHA6Ly95ZTEuaS5sZW5jci5vcmcvMB4GA1UdEQQXMBWCE2luZmVyZW5jZS5waGFsYS5jb20wEwYDVR0gBAwwCjAIBgZngQwBAgEwLwYDVR0fBCgwJjAkoCKgIIYeaHR0cDovL3llMS5jLmxlbmNyLm9yZy8xMjQuY3JsMIIBCwYKKwYBBAHWeQIEAgSB/ASB+QD3AHUAlE5Dh/rswe+B8xkkJqgYZQHH0184AgE/cmd9VTcuGdgAAAGe+JHgaAAABAMARjBEAiA14+XPYjGrDZkqR8dLCVxtHhGwl5ptxQ+muTEMxgASlwIgTIoRIBtE2g12L74hsVIO3NXrnsABXeR94B6I3ExpYQgAfgBs/lAZQ6heqRa8UtEz5NzJHvFBHH0lhCDRc4CeGBjrOgAAAZ74keP0AAgAAAUAEdzdZQQDAEcwRQIhAJV9WKFE5VtU+K9b/Oug3VDsaThlmsB6sxlpvWDCU9HaAiB24O4pNqfuTynlm3HDGNcSK7/GNdz5FBJlEfoiHNH9kTAKBggqhkjOPQQDAwNoADBlAjBatDsgLJ9tuHVePHIu7GgEXKs4iKFw2cw+EQgNHa/q3vE8KDkIpTRd+udKeii9aL0CMQC4BPJGWURCjNL5l4XYPK35Y4LCh9rOOabwjObBFiUI3rXbO0ucgdadUDOzmoBbDlA=";
  const raw = Buffer.from(CERT_DER_BASE64, "base64");
  assert.equal(
    computeSpkiSha256Hex({ raw }),
    "698c87b1ed32d3d67f23d14295fc443e91b82ad4e40482041ac1a9158c8212e2",
  );
});

test("computeSpkiSha256Hex falls back to pubkey and rejects empty certs", () => {
  const pubkey = new Uint8Array([7, 7, 7]);
  assert.equal(computeSpkiSha256Hex({ pubkey }), createHash("sha256").update(pubkey).digest("hex"));
  assert.throws(() => computeSpkiSha256Hex({}), /neither raw DER nor pubkey/);
});

test("hostOfInput + isInferencePath helpers", () => {
  assert.equal(hostOfInput("https://GATEWAY.example/v1/models"), "gateway.example");
  assert.equal(isInferencePath("https://x.example/v1/chat/completions"), true);
  assert.equal(isInferencePath("https://x.example/v1/aci/receipts/1"), false);
});

test("SSE completion: cancel after [DONE] fires sse-done WITHOUT responseBytes (padding is hash-relevant)", async () => {
  const { exchanges, deps } = makeDeps({ failOpenOnUnpinned: () => true });
  const enc = new TextEncoder();
  // The gateway appends keepalive padding after [DONE]; its
  // response.returned.body_hash covers those bytes, so truncated bytes must
  // never be delivered as verifiable responseBytes.
  const stream = "data: chunk1\n\ndata: chunk2\n\ndata: [DONE]\n\n\n\n\n\n";
  const split = [enc.encode(stream.slice(0, 20)), enc.encode(stream.slice(20))];
  const baseFetch = (async () =>
    fakeResponse(streamOf(split), { "x-receipt-id": "rcpt-9" })) as typeof fetch;
  const fetcher = createAciFetch({ ...deps, baseFetch });
  const res = await fetcher(`https://${GATEWAY}/v1/chat/completions`, { method: "POST" });

  // Simulate the AI SDK: read until the [DONE] marker arrives, then cancel
  // instead of reading to EOF.
  const reader = res.body!.getReader();
  let seen = "";
  while (!seen.includes("data: [DONE]")) {
    const { done, value } = await reader.read();
    if (done) break;
    seen += new TextDecoder().decode(value);
  }
  await reader.cancel();
  await new Promise((resolve) => setTimeout(resolve, 10));

  const sseDone = exchanges.filter((x) => x.completion === "sse-done");
  assert.equal(sseDone.length, 1);
  assert.equal(sseDone[0].responseBytes, undefined);
  assert.equal(sseDone[0].headers["x-receipt-id"], "rcpt-9");
  assert.equal(
    exchanges.filter((x) => x.completion === "eof").length,
    0,
    "cancel after [DONE] must not fabricate an eof exchange",
  );
});

test("SSE completion: reading to EOF after [DONE] upgrades with full bytes (padding included)", async () => {
  const { exchanges, deps } = makeDeps({ failOpenOnUnpinned: () => true });
  const enc = new TextEncoder();
  const stream = "data: chunk1\n\ndata: [DONE]\n\n\n\n\n\n";
  const baseFetch = (async () =>
    fakeResponse(streamOf([enc.encode(stream)]), { "x-receipt-id": "rcpt-10" })) as typeof fetch;
  const fetcher = createAciFetch({ ...deps, baseFetch });
  const res = await fetcher(`https://${GATEWAY}/v1/chat/completions`, { method: "POST" });
  await res.text(); // reads to EOF

  const kinds = exchanges.map((x) => x.completion);
  assert.deepEqual(kinds, ["sse-done", "eof"]);
  const eof = exchanges[1];
  assert.equal(new TextDecoder().decode(eof.responseBytes), stream);
});

test("SSE completion: consumer cancelling at the finish_reason frame still fires sse-done (AI SDK behavior)", async () => {
  const { exchanges, deps } = makeDeps({ failOpenOnUnpinned: () => true });
  const enc = new TextEncoder();
  // The AI SDK stops reading at the finish_reason frame and cancels — the
  // [DONE] frame never enters the tee. Completion must still fire.
  const frames = [
    'data: {"choices":[{"delta":{"content":"po"},"finish_reason":null}]}\n\n',
    'data: {"choices":[{"delta":{"content":"ng"},"finish_reason":null}]}\n\n',
    'data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\n',
    "data: [DONE]\n\n\n\n",
  ];
  const baseFetch = (async () =>
    fakeResponse(streamOf(frames.map((f) => enc.encode(f))), {
      "x-receipt-id": "rcpt-11",
    })) as typeof fetch;
  const fetcher = createAciFetch({ ...deps, baseFetch });
  const res = await fetcher(`https://${GATEWAY}/v1/chat/completions`, { method: "POST" });

  const reader = res.body!.getReader();
  let seen = "";
  while (!seen.includes('"finish_reason":"stop"')) {
    const { done, value } = await reader.read();
    if (done) break;
    seen += new TextDecoder().decode(value);
  }
  await reader.cancel();
  await new Promise((resolve) => setTimeout(resolve, 10));

  const sseDone = exchanges.filter((x) => x.completion === "sse-done");
  assert.equal(sseDone.length, 1);
  assert.equal(sseDone[0].headers["x-receipt-id"], "rcpt-11");
  assert.equal(sseDone[0].responseBytes, undefined);
});
