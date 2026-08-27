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

test("verifies a bodyless response before returning it", async () => {
  let verified = false;
  const original = new Response(null, { status: 204 });
  const response = await auditResponse(original, async () => {
    verified = true;
  });

  assert.equal(response, original);
  assert.equal(verified, true);
});
