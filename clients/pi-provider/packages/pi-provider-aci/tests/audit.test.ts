import assert from "node:assert/strict";
import { test } from "node:test";

import { summarizeReceipt, summarizeSession } from "../src/audit.ts";

test("summarizeReceipt keeps the signed routing fields relevant to an audit", () => {
  const lines = summarizeReceipt({
    receipt_id: "rcpt-1",
    api_version: "aci/1",
    key_id: "receipt-ed25519",
    model: "phala/qwen3.5-27b",
    endpoint: "/v1/chat/completions",
    served_at: 1700000000,
    workload_keyset_digest: "sha256:digest",
    event_log: [
      {
        type: "upstream.verified",
        result: "verified",
        required: true,
        provider: "phala",
        model_id: "phala/qwen3.5-27b",
        session_id: "as_123",
      },
      { type: "response.returned", body_hash: "sha256:xxxx" },
      { type: "some.other", ignored: true },
    ],
  });
  const text = lines.join("\n");
  assert.match(text, /Receipt: rcpt-1/);
  assert.match(text, /result=verified required=true provider=phala/);
  assert.match(text, /response.returned body_hash/);
  assert.ok(!text.includes("some.other"));
});

test("summarizeSession renders the attested session identity and lifetime", () => {
  const text = summarizeSession(
    {
      api_version: "aci/1",
      upstream_name: "phala",
      endpoint: "https://inference.phala.com/v1",
      verifier_id: "v1",
      established_at: 1700000000,
      expires_at: 1700600000,
    },
    "as_123",
  ).join("\n");
  assert.match(text, /Session: as_123/);
  assert.match(text, /Upstream: phala/);
  assert.match(text, /Expires:/);
});
