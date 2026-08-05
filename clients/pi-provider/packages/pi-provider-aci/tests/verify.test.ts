import assert from "node:assert/strict";
import { test } from "node:test";

import { generateKeyPairSync, sign as nodeSign } from "node:crypto";

import {
  type Receipt,
  type AttestationReport,
  attestedSpkiSha256ForHost,
  canonicalBytesForSigning,
  classifyReceipt,
  verifyReceipt,
  type ReportBindingResult,
} from "../src/verify.ts";

test("classifyReceipt: verified when result=verified and required=true", () => {
  const receipt: Receipt = {
    workload_id: "sha256:aaa",
    workload_keyset_digest: "sha256:bbb",
    event_log: [
      { type: "upstream.verified", result: "verified", required: true, session_id: "as_1", provider: "phala", model_id: "phala/qwen3.5-27b" },
    ],
  };
  const c = classifyReceipt(receipt);
  assert.equal(c.status, "verified");
  assert.equal(c.sessionId, "as_1");
  assert.equal(c.provider, "phala");
  assert.equal(c.required, true);
  assert.equal(c.workloadId, "sha256:aaa");
});

test("classifyReceipt: routed when result=failed and required=false", () => {
  const receipt: Receipt = {
    workload_id: "sha256:aaa",
    workload_keyset_digest: "sha256:bbb",
    event_log: [
      { type: "upstream.verified", result: "failed", required: false, provider: "openai" },
    ],
  };
  const c = classifyReceipt(receipt);
  assert.equal(c.status, "routed");
  assert.equal(c.required, false);
  assert.equal(c.sessionId, undefined);
});

test("classifyReceipt: unknown when upstream.verified event is missing", () => {
  const receipt: Receipt = {
    workload_id: "sha256:aaa",
    workload_keyset_digest: "sha256:bbb",
    event_log: [{ type: "request.received" }, { type: "response.returned" }],
  };
  const c = classifyReceipt(receipt);
  assert.equal(c.status, "unknown");
});

test("classifyReceipt: unknown when result/required do not match either pattern", () => {
  const receipt: Receipt = {
    event_log: [{ type: "upstream.verified", result: "verified", required: false }],
  };
  const c = classifyReceipt(receipt);
  assert.equal(c.status, "unknown");
});

test("verifyReceipt: workload match true when both workload_id and keyset_digest match", () => {
  const receipt: Receipt = {
    workload_id: "sha256:aaa",
    workload_keyset_digest: "sha256:bbb",
    event_log: [],
  };
  const binding: ReportBindingResult = {
    workloadId: "sha256:aaa",
    workloadKeysetDigest: "sha256:bbb",
    reportData: new Uint8Array(32),
  };
  const attestation: AttestationReport = { attestation: { workload_keyset: {} } };
  const result = verifyReceipt(receipt, binding, attestation);
  assert.equal(result.workloadMatched, true);
});

test("verifyReceipt: workload match false when keyset_digest differs", () => {
  const receipt: Receipt = {
    workload_id: "sha256:aaa",
    workload_keyset_digest: "sha256:bbb",
    event_log: [],
  };
  const binding: ReportBindingResult = {
    workloadId: "sha256:aaa",
    workloadKeysetDigest: "sha256:ccc",
    reportData: new Uint8Array(32),
  };
  const attestation: AttestationReport = { attestation: { workload_keyset: {} } };
  const result = verifyReceipt(receipt, binding, attestation);
  assert.equal(result.workloadMatched, false);
});

test("verifyReceipt: workload match false when fields are missing", () => {
  const receipt: Receipt = { event_log: [] };
  const binding: ReportBindingResult = {
    workloadId: "sha256:aaa",
    workloadKeysetDigest: "sha256:bbb",
    reportData: new Uint8Array(32),
  };
  const attestation: AttestationReport = { attestation: { workload_keyset: {} } };
  const result = verifyReceipt(receipt, binding, attestation);
  assert.equal(result.workloadMatched, false);
});

test("attestedSpkiSha256ForHost: returns the attested SPKI for a matching host", () => {
  const report: AttestationReport = {
    attestation: {
      workload_keyset: {
        tls_public_keys: [
          { domain: "inference.phala.com", spki_sha256: "698c87b1ed32d3d67f23d14295fc443e91b82ad4e40482041ac1a9158c8212e2" },
          { domain: "tee.redpill.ai", spki_sha256: "11af02e1c69bb2227e9b65903010abb60fbb626930cd11b0866281bb291a352c" },
        ],
      },
    },
  };
  assert.equal(
    attestedSpkiSha256ForHost(report, "inference.phala.com"),
    "698c87b1ed32d3d67f23d14295fc443e91b82ad4e40482041ac1a9158c8212e2",
  );
  // Case-insensitive on the host.
  assert.equal(
    attestedSpkiSha256ForHost(report, "INFERENCE.PHALA.COM"),
    "698c87b1ed32d3d67f23d14295fc443e91b82ad4e40482041ac1a9158c8212e2",
  );
  // Unknown host -> undefined.
  assert.equal(attestedSpkiSha256ForHost(report, "example.com"), undefined);
});

test("attestedSpkiSha256ForHost: returns undefined when no tls keys are attested", () => {
  assert.equal(attestedSpkiSha256ForHost({ attestation: {} }, "inference.phala.com"), undefined);
});

test("canonicalBytesForSigning: signs the full wire record minus signature.value", () => {
  const receipt: Receipt = {
    api_version: "aci/1",
    receipt_id: "rcpt-1",
    model: "openai/gpt-oss-20b", // unknown-to-a-projection field must be preserved
    workload_id: "sha256:aaa",
    workload_keyset_digest: "sha256:bbb",
    event_log: [],
    signature: { algo: "ed25519", key_id: "k1", value: "ab12" },
  };
  const bytes = new TextDecoder().decode(canonicalBytesForSigning(receipt));
  assert.ok(bytes.includes('"model":"openai/gpt-oss-20b"'), "model field preserved");
  assert.ok(!bytes.includes('"value"'), "signature.value omitted");
  assert.ok(bytes.includes('"algo":"ed25519"'));
});

test("verifyReceipt: ed25519 receipt signature validates against attested key", () => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const pubRaw = Buffer.from(publicKey.export({ format: "jwk" }).x as string, "base64url");
  const pubHex = pubRaw.toString("hex");

  const receipt: Receipt = {
    api_version: "aci/1",
    receipt_id: "rcpt-ed",
    model: "openai/gpt-oss-20b",
    workload_id: "w",
    workload_keyset_digest: "k",
    served_at: 1700000000,
    event_log: [],
    signature: { algo: "ed25519", key_id: "receipt-ed25519", value: "" },
  };
  const canonical = canonicalBytesForSigning(receipt);
  const sig = nodeSign(null, canonical, privateKey);
  (receipt.signature as { value?: string }).value = Buffer.from(sig).toString("hex");

  const binding: ReportBindingResult = {
    workloadId: "w",
    workloadKeysetDigest: "k",
    reportData: new Uint8Array(0),
  };
  const attestation: AttestationReport = {
    attestation: {
      workload_keyset: {
        receipt_signing_keys: [
          { key_id: "receipt-ed25519", algo: "ed25519", public_key: pubHex },
        ],
      },
    },
  };
  const cls = verifyReceipt(receipt, binding, attestation);
  assert.equal(cls.signatureValid, true);

  // Tamper with the receipt body -> signature no longer validates.
  const tampered: Receipt = { ...receipt, receipt_id: "rcpt-tampered" };
  const cls2 = verifyReceipt(
    { ...tampered, signature: { algo: "ed25519", key_id: "receipt-ed25519", value: (receipt.signature as { value?: string }).value } },
    binding,
    attestation,
  );
  assert.equal(cls2.signatureValid, false);
});
