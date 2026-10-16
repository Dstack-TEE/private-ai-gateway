import { test, mock } from "node:test";
import assert from "node:assert/strict";
import {
  checkOsRegistry,
  type RegistryFetch,
  DEFAULT_OS_REGISTRY_RPC_URLS,
  OS_REGISTRY_CONTRACT,
  osRegistryCallData,
} from "../src/os-registry.js";
import { measuredDstackOs, reportTranscript } from "../src/transcript.js";
import { makeMeasuredComposeReport, NONCE, NOW } from "./fixtures.js";

const HASH = "8db40cb23008d6213668b5816702d100b37b859a8a5bfd18d938ae3a1b3715bd";
const TRUE = `0x${"00".repeat(31)}01`;
const FALSE = `0x${"00".repeat(32)}`;
const URLS = DEFAULT_OS_REGISTRY_RPC_URLS;

function mockRpc(results: unknown[], chains: unknown[] = ["0x1", "0x1"]): RegistryFetch {
  return async (input, init) => {
    const index = URLS.indexOf(String(input));
    assert.ok(index >= 0);
    const body = JSON.parse(String(init?.body)) as Record<string, unknown>;
    assert.equal(init?.method, "POST");
    assert.equal(init?.redirect, "error");
    assert.equal(init?.cache, "no-store");
    if (body.method === "eth_call") {
      assert.deepEqual(body.params, [
        { to: OS_REGISTRY_CONTRACT, data: `0x9a4e1d18${HASH}` },
        "finalized",
      ]);
    } else {
      assert.equal(body.method, "eth_chainId");
      assert.deepEqual(body.params, []);
    }
    return Response.json({
      jsonrpc: "2.0",
      id: body.id,
      result: body.method === "eth_chainId" ? chains[index] : results[index],
    });
  };
}

test("registry: true from all providers passes and every verification reads again", async () => {
  let requests = 0;
  const rpc = mockRpc([TRUE, TRUE]);
  const counted: RegistryFetch = (input, init) => {
    requests++;
    return rpc(input, init);
  };
  const first = checkOsRegistry(HASH, URLS, counted);
  assert.equal(requests, 2, "both chain checks start before either provider responds");
  await first;
  await checkOsRegistry(HASH, URLS, counted);
  assert.equal(requests, 8);
  // An approval is revocable even in the same process.
  await assert.rejects(() => checkOsRegistry(HASH, URLS, mockRpc([FALSE, FALSE])), /not approved/);
});

test("registry: false and disagreement fail closed", async () => {
  await assert.rejects(() => checkOsRegistry(HASH.toUpperCase(), URLS, mockRpc([FALSE, FALSE])), {
    message: `os-image-hash ${HASH} is not approved in the dstack KMS registry (chain 1)`,
  });
  for (const results of [
    [TRUE, FALSE],
    [FALSE, TRUE],
  ]) {
    await assert.rejects(() => checkOsRegistry(HASH, URLS, mockRpc(results)), /providers disagree/);
  }
});

test("registry: exact ABI bool only, and mainnet must be checked on each provider", async () => {
  for (const bad of [
    "0x1",
    `${TRUE}\n`,
    "0x",
    "",
    true,
    null,
    `0x${"00".repeat(31)}02`,
    `0x01${"00".repeat(31)}`,
    `0x${"00".repeat(33)}`,
    `0x${"00".repeat(31)}gg`,
  ]) {
    await assert.rejects(() => checkOsRegistry(HASH, URLS, mockRpc([TRUE, bad])));
  }
  for (const chains of [
    ["0x2", "0x1"],
    ["0x1", "0x2"],
    ["0x01", "0x1"],
  ]) {
    await assert.rejects(
      () => checkOsRegistry(HASH, URLS, mockRpc([TRUE, TRUE], chains)),
      /chain 1/,
    );
  }
});

test("registry: network, HTTP, JSON-RPC errors, invalid JSON, wrong ids and oversized bodies fail closed", async () => {
  const responses = [
    () => Promise.reject(new Error("private provider URL")),
    () => Promise.resolve(new Response("private details", { status: 503 })),
    () =>
      Promise.resolve(
        Response.json({ jsonrpc: "2.0", id: 1, error: { message: "private details" } }),
      ),
    () => Promise.resolve(Response.json({ jsonrpc: "2.0", id: 2, result: "0x1" })),
    () => Promise.resolve(new Response("not JSON")),
    () => Promise.resolve(new Response(" ".repeat(4097))),
  ];
  for (const rpc of responses) {
    await assert.rejects(
      () => checkOsRegistry(HASH, URLS, rpc),
      (error: Error) => {
        assert.ok(!error.message.includes("private"));
        return true;
      },
    );
  }
});

test("registry: native timeout signal fails closed", async () => {
  const signal = AbortSignal.abort(new DOMException("deadline", "TimeoutError"));
  const timeout = mock.method(AbortSignal, "timeout", (milliseconds: number) => {
    assert.equal(milliseconds, 10_000);
    return signal;
  });
  try {
    await assert.rejects(
      () =>
        checkOsRegistry(HASH, URLS, async (_input, init) => {
          assert.equal(init?.signal, signal);
          signal.throwIfAborted();
          throw new Error("aborted fetch must not complete");
        }),
      /timed out/,
    );
    assert.equal(timeout.mock.callCount(), URLS.length);
  } finally {
    timeout.mock.restore();
  }
});

test("registry: bytes32 ABI encoding and HTTPS provider policy", async () => {
  assert.equal(osRegistryCallData(HASH.toUpperCase()), `0x9a4e1d18${HASH}`);
  for (const hash of ["ab", `0x${HASH}`, `${HASH}\n`, "gg".repeat(32), "00".repeat(33)]) {
    assert.throws(() => osRegistryCallData(hash));
  }
  for (const urls of [
    [],
    ["https://a.example"],
    ["https://a.example", "http://b.example"],
    ["https://a.example/rpc", "https://a.example/other"],
    ["https://a.example", "https://user:secret@b.example"],
    ["https://a.example", "https://b.example/#fragment"],
  ]) {
    let called = false;
    await assert.rejects(() =>
      checkOsRegistry(HASH, urls, async () => {
        called = true;
        throw new Error();
      }),
    );
    assert.equal(called, false);
  }
});

test("transcript: registry requires both verified hardware and RTMR3 provenance, without RPC calls", async () => {
  const { report } = await makeMeasuredComposeReport();
  let requests = 0;
  const stubFetch = mock.method(globalThis, "fetch", async () => {
    requests++;
    throw new Error("must not fetch");
  });
  try {
    const transcript = await reportTranscript(report, NONCE, {
      now: NOW,
      requireProductionOs: true,
    });
    assert.equal(transcript.lines.find((line) => line.id === "id-4")?.status, "pass");
    assert.equal(transcript.lines.find((line) => line.id === "id-1")?.status, "fail");
    assert.equal(transcript.lines.find((line) => line.id === "policy-os")?.status, "fail");
    assert.ok(transcript.lines.find((line) => line.id === "policy-os")?.detail?.includes("id-1"));
    assert.equal(requests, 0);
  } finally {
    stubFetch.mock.restore();
  }
});

test("OS event: digest is recomputed from the payload, independent of the registry", async () => {
  const { report } = await makeMeasuredComposeReport();
  // Real dstack-0.5.9 payload and RTMR3 event digest.
  const hash = "bd369a8c2f9edb2b52dad48ac8e0b32dde5f1337c423a506b48d07403a7d8033";
  const event = {
    imr: 3,
    event_type: 0x08000001,
    event: "os-image-hash",
    event_payload: hash.toUpperCase(),
    digest:
      "c936f709860155a455c519b505fe29bde21d1b778534a8f0b41b706b0f789eae2ea899e224fedb2331c18324aab4db93",
  };
  report.attestation.evidence = { event_log: JSON.stringify([event]) };
  assert.equal(await measuredDstackOs(report), hash);
  report.attestation.evidence = {
    event_log: JSON.stringify([{ ...event, event_payload: "00".repeat(32) }]),
  };
  await assert.rejects(() => measuredDstackOs(report), /event digest/);
  report.attestation.evidence = { event_log: JSON.stringify([event, event]) };
  await assert.rejects(() => measuredDstackOs(report), /multiple/);
  report.attestation.evidence = {
    event_log: JSON.stringify([
      {
        imr: 3,
        event_type: 0x08000001,
        event: "system-ready",
        digest: "00".repeat(48),
        event_payload: "",
      },
      event,
    ]),
  };
  await assert.rejects(() => measuredDstackOs(report), /no os-image-hash/);
});
