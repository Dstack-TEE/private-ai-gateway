// Backward-compatibility shim over the ACI verifier client.
//
// The provider's verification logic lives in `aci-client.ts`, which wraps the
// repo's reference verifier `@phala/aci-verifier` (clients/verifier-ts). This
// module re-exports the names earlier versions of the provider imported from
// ./verify so consumers (index.ts, tests) keep a stable import surface.
//
// The old hand-rolled binder was a private re-implementation of the protocol
// and drifted from the spec (top-level `workload_id`, `keyset_endorsement`,
// object-shaped receipt signatures). Everything here now resolves to the
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

export type {
  ReceiptClassification,
  ReceiptStatus,
} from "./aci-client.ts";