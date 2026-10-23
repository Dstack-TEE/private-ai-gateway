import assert from "node:assert/strict";
import { test } from "node:test";

import { DEGENERATE_RUN_LENGTH, classifyReplyFailure, scanRepetition } from "./reply-failure.mjs";
import { runReply } from "./reply-process.mjs";

test("detects a degenerate run across chunk boundaries, counting Unicode characters", () => {
  for (const character of ["!", "a", "🤖"]) {
    const previous = scanRepetition(undefined, `reasoning: ${character.repeat(DEGENERATE_RUN_LENGTH - 1)}`);
    assert.deepEqual(previous, { character, length: DEGENERATE_RUN_LENGTH - 1 });
    assert.deepEqual(scanRepetition(previous, character), { character, length: DEGENERATE_RUN_LENGTH });
    assert.deepEqual(previous, { character, length: DEGENERATE_RUN_LENGTH - 1 });
  }
});

test("normal replies, prompt echoes, whitespace and long varied output are not degenerate", () => {
  for (const text of [
    "PAP-OK\n", "**PAP-OK**", "Reply with exactly: PAP-OK\nPAP-OK\n",
    " \t\r\n".repeat(1_000), "abc!?🤖".repeat(10_000),
    `${"!".repeat(499)}\n${"!".repeat(499)}`, "Loading...\r".repeat(1_000),
  ]) {
    assert.ok(scanRepetition(undefined, text).length < DEGENERATE_RUN_LENGTH);
  }
  const previous = scanRepetition(undefined, "!".repeat(499));
  assert.deepEqual(scanRepetition(previous, "?"), { character: "?", length: 1 });
  assert.deepEqual(scanRepetition(previous, "\n"), { character: "", length: 0 });
});

const failure = { model: "test/model", timeout: 240_000, status: null, signal: "SIGKILL" };

test("degenerate output is an upstream failure with the model, character and length", () => {
  const { upstream, reason } = classifyReplyFailure({
    ...failure, degenerate: { character: "!", length: 500 },
  });
  assert.equal(upstream, true);
  assert.match(reason, /upstream model test\/model emitted degenerate output/);
  assert.match(reason, /"!" repeated 500 times/);
});

test("only a timed-out HTTP 200 request with verification pending is an unfinished upstream response", () => {
  const timeout = { ...failure, error: { code: "ETIMEDOUT" } };
  const { upstream, reason } = classifyReplyFailure({ ...timeout, record: { status: 200, verified: null } });
  assert.equal(upstream, true);
  assert.match(reason, /upstream model did not finish its response \(stream still open after 240 s\)/);
  assert.match(reason, /test\/model/);

  for (const record of [
    undefined, null, { status: 200 }, { status: 200, verified: true }, { status: 200, verified: false },
    { status: 503, verified: null }, { status: 0, verified: null },
  ]) {
    assert.deepEqual(classifyReplyFailure({ ...timeout, record }), {
      upstream: false, reason: "timed out after 240 s",
    });
  }
  const record = { status: 200, verified: null };
  assert.deepEqual(classifyReplyFailure({ ...failure, status: 1, record }), {
    upstream: false, reason: "exited 1 without replying PAP-OK",
  });
  assert.deepEqual(classifyReplyFailure({ ...failure, record, error: { code: "ENOENT", message: "not found" } }), {
    upstream: false, reason: "did not finish: not found",
  });
});

test("the async reply runner captures both streams with the supplied environment", async () => {
  const result = await runReply(process.execPath, ["-e", `
    console.log(process.env.PAP_E2E_FIXTURE);
    console.error("diagnostic");
  `], { env: { PAP_E2E_FIXTURE: "PAP-OK" }, timeout: 5_000 });
  assert.equal(result.status, 0);
  assert.equal(result.stdout, "PAP-OK\n");
  assert.equal(result.stderr, "diagnostic\n");
  assert.equal(result.error, undefined);
  assert.equal(result.degenerate, undefined);
});

test("the async reply runner stops a degenerate stream even after PAP-OK was printed", async () => {
  const result = await runReply(process.execPath, ["-e", `
    console.log("PAP-OK");
    process.stderr.write("reasoning: " + "!".repeat(300));
    setTimeout(() => process.stderr.write("!".repeat(300)), 20);
    setInterval(() => {}, 1_000);
  `], { timeout: 5_000 });
  assert.deepEqual(result.degenerate, { character: "!", length: 500 });
  assert.equal(result.error, undefined);
  assert.equal(result.status, null);
  assert.equal(result.signal, "SIGKILL");
  assert.equal(result.stdout, "PAP-OK\n");
  assert.equal(result.stderr, `reasoning: ${"!".repeat(600)}`);
});

test("the async reply timeout stops descendants that hold the output pipes open", async () => {
  const result = await runReply(process.execPath, ["-e", `
    const { spawn } = require("node:child_process");
    spawn(process.execPath, ["-e", 'setTimeout(() => console.log("descendant survived"), 1_000)'], {
      stdio: ["ignore", "inherit", "inherit"],
    });
    console.log("waiting");
    console.error("pending");
    setInterval(() => {}, 1_000);
  `], { timeout: 300 });
  assert.equal(result.error?.code, "ETIMEDOUT");
  assert.equal(result.degenerate, undefined);
  assert.equal(result.status, null);
  assert.equal(result.signal, "SIGKILL");
  assert.equal(result.stdout, "waiting\n");
  assert.equal(result.stderr, "pending\n");
});

test("the async reply runner reports a command that cannot start", async () => {
  const result = await runReply("/pap-e2e-no-such-command", [], { timeout: 5_000 });
  assert.equal(result.error?.code, "ENOENT");
  assert.equal(result.stdout, "");
  assert.equal(result.stderr, "");
});
