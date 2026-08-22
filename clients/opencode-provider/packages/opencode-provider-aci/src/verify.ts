// Stable import surface over the ACI verifier client.
//
// The provider's verification logic lives in `aci-client.ts`, which wraps the
// repo's reference verifier `@phala/aci-verifier` (clients/verifier-ts). This
// module re-exports the names consumers (index.ts, tests) need from ./verify
// so the verifier wiring stays in one place. Everything resolves to the
// reference verifier, which is conformance-tested against spec/test-vectors.md.

export {
  type AttestationReport,
  type ReceiptEnvelope,
  type ReportVerification,
  type WorkloadKeyset,
} from "@phala/aci-verifier";

export {
  attestedSpkiSha256ForHost,
  bindAttestation,
  canonicalRequestBytes,
  classifyReceipt,
  fetchAttestation,
  fetchReceipt,
  fetchSession,
  isFullyVerified,
  keysetStaleAfterMs,
  newNonce,
  receiptSigningKeys,
} from "./aci-client.ts";

export type { ReceiptClassification, ReceiptStatus } from "./aci-client.ts";
