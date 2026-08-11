import assert from "node:assert/strict";
import { test } from "node:test";

import type { AttestationReport, WorkloadKeyset } from "../src/verify.ts";
import { attestedSpkiSha256ForHost, keysetStaleAfterMs } from "../src/verify.ts";

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
  assert.equal(
    attestedSpkiSha256ForHost(makeKeyset({ tls_public_keys: undefined }), "inference.phala.com"),
    undefined,
  );
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

test("attestedSpkiSha256ForHost on a full report-shaped keyset is not required; keyset arg is used", () => {
  const report = makeReport(makeKeyset());
  // The function consumes keyset now (not a whole report). Just confirm it
  // still digs through the TLS pins on the keyset object.
  const keyset = report.attestation.workload_keyset as WorkloadKeyset;
  assert.ok(attestedSpkiSha256ForHost(keyset, "inference.phala.com"));
});