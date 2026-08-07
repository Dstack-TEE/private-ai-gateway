import assert from "node:assert/strict";
import { test } from "node:test";

import { generateKeyPairSync, sign as nodeSign } from "node:crypto";
import { jcsBytes } from "@phala/aci-verifier";

import {
  type AttestationReport,
  type ReceiptEnvelope,
  type WorkloadKeyset,
  attestedSpkiSha256ForHost,
  classifyReceipt,
  keysetStaleAfterMs,
} from "../src/verify.ts";

function ed25519Pair() {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const pubRaw = Buffer.from(publicKey.export({ format: "jwk" }).x as string, "base64url");
  return { pubRaw, pubHex: pubRaw.toString("hex"), privateKey };
}

/** A keyset matching the spec §3.1 shape (flat, with not_after + TLS pins). */
function makeKeyset(over: Partial<WorkloadKeyset> = {}): WorkloadKeyset {
  return {
    not_after: 2000000000,
    receipt_signing_keys: [],
    e2ee_public_keys: [],
    tls_public_keys: [
      { domain: "inference.phala.com", spki_sha256: "698c87b1ed32d3d67f23d14295fc443e91b82ad4e40482041ac1a9158c8212e2" },
      { domain: "tee.redpill.ai", spki_sha256: "11af02e1c69bb2227e9b65903010abb60fbb626930cd11b0866281bb291a352c" },
    ],
    ...over,
  };
}

function makeReport(keyset: WorkloadKeyset): AttestationReport {
  return {
    api_version: "aci/1",
    workload_keyset_digest: "sha256:digest",
    attestation: { tee_type: "tdx", workload_keyset: keyset, report_data: "00".repeat(32) },
  };
}

/** A receipt envelope per §7.2/§7.3: top-level key_id + hex signature over
 *  JCS(document minus signature). */
function makeReceipt(over: Partial<ReceiptEnvelope> = {}): ReceiptEnvelope {
  return {
    api_version: "aci/1",
    receipt_id: "rcpt-1",
    model: "openai/gpt-oss-20b",
    workload_keyset_digest: "sha256:digest",
    endpoint: "/v1/chat/completions",
    method: "POST",
    served_at: 1700000000,
    event_log: [],
    key_id: "receipt-ed25519",
    signature: "",
    ...over,
  };
}

async function signReceipt(receipt: ReceiptEnvelope, privateKey: Buffer): Promise<ReceiptEnvelope> {
  // §7.2: the signature signs JCS(document minus `signature`) under Ed25519
  // (RFC 8032 — raw message, no prehash).
  const { signature: _sig, ...unsigned } = receipt;
  const unsignedBytes = jcsBytes(unsigned);
  const sig = nodeSign(null, unsignedBytes, privateKey);
  return { ...receipt, signature: Buffer.from(sig).toString("hex") };
}

test("classifyReceipt: verified when result=verified and required=true", async () => {
  const keyset = makeKeyset();
  const receipt: ReceiptEnvelope = {
    ...makeReceipt(),
    event_log: [
      { type: "upstream.verified", result: "verified", required: true, session_id: "as_1", provider: "phala", model_id: "phala/qwen3.5-27b" },
    ],
  };
  const c = await classifyReceipt(receipt, keyset, "sha256:digest");
  assert.equal(c.status, "verified");
  assert.equal(c.sessionId, "as_1");
  assert.equal(c.provider, "phala");
  assert.equal(c.required, true);
  assert.equal(c.modelId, "openai/gpt-oss-20b");
});

test("classifyReceipt: routed when result=failed and required=false", async () => {
  const keyset = makeKeyset();
  const receipt: ReceiptEnvelope = {
    ...makeReceipt(),
    event_log: [
      { type: "upstream.verified", result: "failed", required: false, provider: "openai" },
    ],
  };
  const c = await classifyReceipt(receipt, keyset, "sha256:digest");
  assert.equal(c.status, "routed");
  assert.equal(c.required, false);
  assert.equal(c.sessionId, undefined);
});

test("classifyReceipt: unknown when upstream.verified event is missing", async () => {
  const keyset = makeKeyset();
  const receipt: ReceiptEnvelope = {
    ...makeReceipt(),
    event_log: [{ type: "request.received" }, { type: "response.returned" }],
  };
  const c = await classifyReceipt(receipt, keyset, "sha256:digest");
  assert.equal(c.status, "unknown");
});

test("classifyReceipt: signatureValid false when key_id not in keyset", async () => {
  const keyset = makeKeyset(); // receipt_signing_keys empty
  const receipt = makeReceipt();
  const c = await classifyReceipt(receipt, keyset, "sha256:digest");
  assert.equal(c.signatureValid, false);
});

test("classifyReceipt: ed25519 receipt signature validates against attested key (spec shape)", async () => {
  const { pubHex, privateKey } = ed25519Pair();
  const keyset = makeKeyset({
    receipt_signing_keys: [{ key_id: "receipt-ed25519", algo: "ed25519", public_key: pubHex }],
  });
  const receipt = await signReceipt(
    makeReceipt({ key_id: "receipt-ed25519" }),
    privateKey as unknown as Buffer,
  );
  assert.ok(receipt.signature.length > 0, "receipt signed");

  const c = await classifyReceipt(receipt, keyset, "sha256:digest");
  assert.equal(c.signatureValid, true);
});

test("classifyReceipt: tampered receipt body invalidates the signature", async () => {
  const { pubHex, privateKey } = ed25519Pair();
  const keyset = makeKeyset({
    receipt_signing_keys: [{ key_id: "receipt-ed25519", algo: "ed25519", public_key: pubHex }],
  });
  const signed = await signReceipt(makeReceipt({ key_id: "receipt-ed25519" }), privateKey as unknown as Buffer);
  const tampered: ReceiptEnvelope = { ...signed, receipt_id: "rcpt-tampered" };
  const c = await classifyReceipt(tampered, keyset, "sha256:digest");
  assert.equal(c.signatureValid, false);
});

test("classifyReceipt: body hashes are checked when bytes provided; hashesChecked false otherwise", async () => {
  const { pubHex, privateKey } = ed25519Pair();
  const keyset = makeKeyset({
    receipt_signing_keys: [{ key_id: "receipt-ed25519", algo: "ed25519", public_key: pubHex }],
  });
  const requestBytes = new TextEncoder().encode("the-request-body");
  const receipt = await signReceipt(
    makeReceipt({
      key_id: "receipt-ed25519",
      event_log: [],
      // No body_hash events -> checks report false when bytes provided.
    }),
    privateKey as unknown as Buffer,
  );

  const withoutBytes = await classifyReceipt(receipt, keyset, "sha256:digest");
  assert.equal(withoutBytes.hashesChecked, false);
  assert.ok(withoutBytes.hashesNotCheckedReason);
  assert.equal(withoutBytes.requestHashValid, undefined);
  assert.equal(withoutBytes.responseHashValid, undefined);

  const withBytes = await classifyReceipt(receipt, keyset, "sha256:digest", { requestBody: requestBytes });
  assert.equal(withBytes.hashesChecked, true);
  assert.equal(withBytes.requestHashValid, false); // event absent -> no match
});

test("attestedSpkiSha256ForHost: returns the attested SPKI for a matching host", () => {
  const keyset = makeKeyset();
  assert.equal(
    attestedSpkiSha256ForHost(keyset, "inference.phala.com"),
    "698c87b1ed32d3d67f23d14295fc443e91b82ad4e40482041ac1a9158c8212e2",
  );
  // Case-insensitive on the host.
  assert.equal(
    attestedSpkiSha256ForHost(keyset, "INFERENCE.PHALA.COM"),
    "698c87b1ed32d3d67f23d14295fc443e91b82ad4e40482041ac1a9158c8212e2",
  );
  // Unknown host -> undefined.
  assert.equal(attestedSpkiSha256ForHost(keyset, "example.com"), undefined);
});

test("attestedSpkiSha256ForHost: returns undefined when no tls keys are attested", () => {
  assert.equal(attestedSpkiSha256ForHost(makeKeyset({ tls_public_keys: undefined }), "inference.phala.com"), undefined);
});

test("keysetStaleAfterMs: reads not_after from the keyset (verifier-ts report freshness)", () => {
  const keyset = makeKeyset({ not_after: 2000000000 });
  assert.equal(keysetStaleAfterMs(keyset), 2000000000 * 1000);
  assert.equal(keysetStaleAfterMs(undefined), undefined);
});

test("bindAttestation fails closed on a mismatched report (no throw)", async () => {
  // bindAttestation is a provider-side wrapper around the reference verifier's
  // verifyReportBinding; assert the reference returns ok:false (not throw) for
  // a report whose digest is not recomputed from its own keyset.
  const { verifyReportBinding } = await import("@phala/aci-verifier");
  const keysetValue = makeKeyset();
  const nonce = "a".repeat(64);
  const bad = makeReport(keysetValue); // api_version aci/1 but digest not recomputed
  const verification = await verifyReportBinding(bad, nonce);
  assert.equal(verification.ok, false);
});

test("footerText: a FAILED signature renders 'mismatch', not 'verified*' (m6)", async () => {
  const { footerText, AciReceiptStore } = await import("../src/receipt-store.ts");
  const store = new AciReceiptStore();
  // Simulate a classification where the signature check explicitly failed.
  store.recordResponseHeaders({ "x-receipt-id": "rcpt-1", "x-aci-identity": "w", "x-aci-keyset-digest": "k" });
  (store as unknown as { lastClassification: Record<string, unknown> }).lastClassification = {
    status: "unknown",
    signatureValid: false,
    hashesChecked: true,
  };
  assert.equal(footerText(store), "aci: mismatch");
});

test("footerText: verified requires hashes checked; un-checked hashes show verified* (M2)", async () => {
  const { footerText, AciReceiptStore } = await import("../src/receipt-store.ts");
  const store = new AciReceiptStore();
  store.recordResponseHeaders({ "x-receipt-id": "rcpt-1" });
  (store as unknown as { lastClassification: Record<string, unknown> }).lastClassification = {
    status: "verified",
    signatureValid: true,
    hashesChecked: false,
  };
  assert.equal(footerText(store), "aci: verified*");
});

test("footerText: fully verified (signature + hashes) renders 'verified'", async () => {
  const { footerText, AciReceiptStore } = await import("../src/receipt-store.ts");
  const store = new AciReceiptStore();
  store.recordResponseHeaders({ "x-receipt-id": "rcpt-1" });
  (store as unknown as { lastClassification: Record<string, unknown> }).lastClassification = {
    status: "verified",
    signatureValid: true,
    hashesChecked: true,
    requestHashValid: true,
    responseHashValid: true,
  };
  assert.equal(footerText(store), "aci: verified");
});

test("attestedSpkiSha256ForHost on a full report-shaped keyset is not required; keyset arg is used", () => {
  const report = makeReport(makeKeyset());
  // The function consumes keyset now (not a whole report). Just confirm it
  // still digs through the TLS pins on the keyset object.
  const keyset = report.attestation.workload_keyset as WorkloadKeyset;
  assert.ok(attestedSpkiSha256ForHost(keyset, "inference.phala.com"));
});