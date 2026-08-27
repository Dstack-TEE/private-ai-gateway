import assert from "node:assert/strict";
import test from "node:test";

import { computeSessionId } from "@phala/aci-verifier";

import { auditAciSession } from "../src/session.ts";

const session = {
  api_version: "aci/1",
  upstream_name: "example",
  endpoint: "https://upstream.example/v1",
  verifier_id: "example/1",
  established_at: 1_750_000_000,
  expires_at: 1_750_003_600,
  channel_binding: [],
  claims: {},
  evidence: {
    data: "data:text/plain;base64,ZXhhbXBsZS1ldmlkZW5jZQ==",
    digest: "sha256:80d70e44d0ae1e829fd5f37c3ee4a60dfbea8d3aa18407ea3f34cf7ec91da34d",
  },
};

test("audits a content-addressed ACI session", async () => {
  const sessionId = await computeSessionId(session);
  const audit = await auditAciSession(sessionId, session);

  assert.equal(audit.verified, true);
  assert.deepEqual(
    audit.checks.map((check) => check.name),
    ["content-address", "api-version", "validity-window", "evidence"],
  );
});

test("rejects malformed timestamps and fails a reversed validity window", async () => {
  await assert.rejects(
    auditAciSession("a".repeat(64), { ...session, established_at: Number.NaN }),
    /invalid established_at/,
  );

  const reversed = { ...session, expires_at: session.established_at - 1 };
  const audit = await auditAciSession(await computeSessionId(reversed), reversed);
  assert.equal(audit.verified, false);
  assert.equal(audit.checks.find((check) => check.name === "validity-window")?.ok, false);
});
