// Attestation client re-exports.
//
// The provider's attestation/binding logic lives in `aci-client.ts`, which
// wraps the repo's reference verifier `@phala/aci-verifier` (clients/
// verifier-ts). Per-response receipt verification is intentionally not part
// of this plugin: pinning is the prevention control, and the receipt audit
// was removed on request.
//
// This module keeps one import surface for consumers (index.ts, tests).

export {
  type AttestationReport,
  type ReportVerification,
  type WorkloadKeyset,
} from "@phala/aci-verifier";

export {
  attestedSpkiSha256ForHost,
  bindAttestation,
  fetchAttestation,
  fetchReceipt,
  fetchSession,
  keysetStaleAfterMs,
  newNonce,
  receiptSigningKeys,
  summarizeReceipt,
  summarizeSession,
} from "./aci-client.ts";