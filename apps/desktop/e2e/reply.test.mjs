import assert from "node:assert/strict";
import { test } from "node:test";

import { classifyReplyFailure, latestReplyUsage, repeatedOutput } from "./reply-failure.mjs";

test("repetition diagnostics report the full longest run, counting Unicode characters", () => {
  assert.deepEqual(repeatedOutput(`reasoning: ${"!".repeat(3_412)}\nfinished`), { character: "!", length: 3_412 });
  assert.deepEqual(repeatedOutput(`${"🤖".repeat(750)}\n${"!".repeat(500)}`), { character: "🤖", length: 750 });
});

test("normal replies, prompt echoes, whitespace and long varied output have no repetition diagnostic", () => {
  for (const text of [
    "PAP-OK\n", "**PAP-OK**", "Reply with exactly: PAP-OK\nPAP-OK\n",
    " \t\r\n".repeat(1_000), "abc!?🤖".repeat(10_000),
    `${"!".repeat(499)}\n${"!".repeat(499)}`, "Loading...\r".repeat(1_000),
  ]) {
    assert.equal(repeatedOutput(text), undefined);
  }
});

const failure = { model: "test/model", reply: "PAP-OK", timeout: 240_000, status: null, signal: "SIGKILL" };

test("base64 values, separators and progress bars are informational and never trigger UPSTREAM", () => {
  for (const [text, character, length] of [
    [JSON.stringify({ data: Buffer.alloc(512).toString("base64") }), "A", 683],
    ["-".repeat(1_000), "-", 1_000],
    [`[${"█".repeat(600)}] 100%`, "█", 600],
  ]) {
    const result = { ...failure, stdout: `${text}\nPAP-OK\n`, stderr: "" };
    assert.deepEqual(repeatedOutput(result.stdout), { character, length });
    assert.equal(classifyReplyFailure({ ...result, status: 0 }).upstream, false);
    assert.deepEqual(classifyReplyFailure({ ...result, error: { code: "ETIMEDOUT" } }), {
      upstream: false, reason: "timed out after 240 s",
    });
  }
});

test("only a timed-out HTTP 200 request with verification pending is an unfinished upstream response", () => {
  const timeout = { ...failure, error: { code: "ETIMEDOUT" } };
  const { upstream, reason } = classifyReplyFailure({ ...timeout, record: { leftDevice: true, status: 200, verified: null } });
  assert.equal(upstream, true);
  assert.match(reason, /upstream model did not finish its response \(stream still open after 240 s\)/);
  assert.match(reason, /test\/model/);

  for (const record of [
    undefined, null, { status: 200, verified: null }, { leftDevice: false, status: 200, verified: null },
    { leftDevice: true, status: 200 }, { leftDevice: true, status: 200, verified: true },
    { leftDevice: true, status: 200, verified: false }, { leftDevice: true, status: 503, verified: null },
    { leftDevice: true, status: 0, verified: null },
  ]) {
    assert.deepEqual(classifyReplyFailure({ ...timeout, record }), {
      upstream: false, reason: "timed out after 240 s",
    });
  }
  const record = { leftDevice: true, status: 200, verified: null };
  assert.deepEqual(classifyReplyFailure({ ...failure, status: 1, record }), {
    upstream: false, reason: "exited 1 without replying PAP-OK",
  });
  assert.deepEqual(classifyReplyFailure({ ...failure, record, error: { code: "ENOENT", message: "not found" } }), {
    upstream: false, reason: "did not finish: not found",
  });
});

test("a pre-existing same-second pending record cannot classify a reply timeout as UPSTREAM", () => {
  const old = { id: "old", at: 123, leftDevice: true, status: 200, verified: null };
  const previousIds = new Set([old.id]);
  const timeout = { ...failure, error: { code: "ETIMEDOUT" } };
  assert.equal(latestReplyUsage([old], previousIds), undefined);
  assert.deepEqual(classifyReplyFailure({ ...timeout, record: latestReplyUsage([old], previousIds) }), {
    upstream: false, reason: "timed out after 240 s",
  });

  const fresh = { id: "new", at: 123, leftDevice: true, status: 200, verified: null };
  const record = latestReplyUsage([old, fresh], previousIds);
  assert.equal(record, fresh);
  assert.equal(classifyReplyFailure({ ...timeout, record }).upstream, true);

  const settled = { ...fresh, verified: true };
  assert.equal(classifyReplyFailure({ ...timeout, record: latestReplyUsage([settled, old], previousIds) }).upstream, false);
});
