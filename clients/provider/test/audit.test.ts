import assert from "node:assert/strict";
import test from "node:test";

import { auditResponse } from "../src/provider.ts";

test("holds response completion until receipt verification succeeds", async () => {
  let verified = false;
  const response = await auditResponse(new Response("complete"), async () => {
    verified = true;
  });

  assert.equal(await response.text(), "complete");
  assert.equal(verified, true);
});

test("turns a failed receipt audit into a response stream error", async () => {
  const response = await auditResponse(new Response("untrusted"), async () => {
    throw new Error("receipt rejected");
  });

  await assert.rejects(response.text(), /receipt rejected/);
});

test("waits for receipt verification when the consumer cancels the stream", async () => {
  let verified = 0;
  let releaseVerification = () => {};
  let signalVerificationStarted = () => {};
  const verificationBlocked = new Promise<void>((resolve) => {
    releaseVerification = resolve;
  });
  const verificationStarted = new Promise<void>((resolve) => {
    signalVerificationStarted = resolve;
  });
  let cancellationReason: unknown;
  const source = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode("partial"));
    },
    cancel(reason) {
      cancellationReason = reason;
    },
  });
  const response = await auditResponse(new Response(source), async () => {
    signalVerificationStarted();
    await verificationBlocked;
    verified += 1;
  });
  const reader = response.body?.getReader();
  assert.ok(reader);

  await reader.read();
  let cancellationSettled = false;
  const cancellation = reader.cancel("consumer finished").then(() => {
    cancellationSettled = true;
  });

  await verificationStarted;
  assert.equal(cancellationSettled, false);
  releaseVerification();
  await cancellation;

  assert.equal(verified, 1);
  assert.equal(cancellationReason, "consumer finished");
});

test("rejects stream cancellation when receipt verification fails", async () => {
  const source = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(new TextEncoder().encode("untrusted"));
    },
  });
  const response = await auditResponse(new Response(source), async () => {
    throw new Error("receipt rejected");
  });
  const reader = response.body?.getReader();
  assert.ok(reader);

  await reader.read();
  await assert.rejects(reader.cancel(), /receipt rejected/);
});

test("verifies a bodyless response before returning it", async () => {
  let verified = false;
  const original = new Response(null, { status: 204 });
  const response = await auditResponse(original, async () => {
    verified = true;
  });

  assert.equal(response, original);
  assert.equal(verified, true);
});
