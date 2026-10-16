import assert from "node:assert/strict";
import { test } from "node:test";

import {
  auditCatalogModels,
  classifySessionList,
  unauditableModelMessage,
} from "../src/model-audit.ts";

const entry = (digest: string | undefined) => ({ evidence: { digest } });

test("classifySessionList: empty or unreadable list is unknown", () => {
  assert.equal(classifySessionList({ sessions: [] }), "unknown");
  assert.equal(classifySessionList({}), "unknown");
  assert.equal(classifySessionList(null), "unknown");
  assert.equal(classifySessionList("garbage"), "unknown");
});

test("classifySessionList: all sessions with evidence digest are auditable", () => {
  assert.equal(
    classifySessionList({ sessions: [entry("sha256:aa"), entry("sha256:bb")] }),
    "auditable",
  );
});

test("classifySessionList: any session missing the digest is unauditable", () => {
  assert.equal(classifySessionList({ sessions: [entry(undefined)] }), "unauditable");
  assert.equal(classifySessionList({ sessions: [entry("")] }), "unauditable");
  assert.equal(
    classifySessionList({ sessions: [entry("sha256:aa"), entry(undefined)] }),
    "unauditable",
  );
  assert.equal(classifySessionList({ sessions: [{}] }), "unauditable");
  assert.equal(classifySessionList({ sessions: [null] }), "unauditable");
});

test("auditCatalogModels: classifies every model and never rejects", async () => {
  const calls: string[] = [];
  const fetch = (async (input: RequestInfo | URL) => {
    const url = String(input);
    calls.push(url);
    if (url.includes("good")) {
      return new Response(JSON.stringify({ sessions: [entry("sha256:aa")] }), {
        status: 200,
      });
    }
    if (url.includes("bad")) {
      return new Response(JSON.stringify({ sessions: [entry(undefined)] }), {
        status: 200,
      });
    }
    if (url.includes("empty")) {
      return new Response(JSON.stringify({ sessions: [] }), { status: 200 });
    }
    return new Response("nope", { status: 500 });
  }) as typeof globalThis.fetch;

  const audit = await auditCatalogModels({
    baseUrl: "https://gateway.example/v1/",
    fetch,
    modelIds: ["good/model", "bad/model", "empty/model", "failing/model"],
    concurrency: 2,
    timeoutMs: 1_000,
  });

  assert.deepEqual(
    [...audit.entries()].sort(),
    [
      ["bad/model", "unauditable"],
      ["empty/model", "unknown"],
      ["failing/model", "unknown"],
      ["good/model", "auditable"],
    ].sort(),
  );
  // baseUrl trailing slash must not produce a double slash.
  assert.ok(calls.every((url) => !url.includes("//aci")));
  assert.ok(calls.every((url) => url.includes("model=")));
});

test("auditCatalogModels: fetch rejection degrades to unknown", async () => {
  const fetch = (async () => {
    throw new Error("network down");
  }) as typeof globalThis.fetch;
  const audit = await auditCatalogModels({
    baseUrl: "https://gateway.example/v1",
    fetch,
    modelIds: ["any/model"],
  });
  assert.equal(audit.get("any/model"), "unknown");
});

test("unauditableModelMessage names the model and the responsible side", () => {
  const message = unauditableModelMessage("Phala Cloud", "moonshotai/kimi-k3");
  assert.match(message, /moonshotai\/kimi-k3/);
  assert.match(message, /gateway-side/);
});
