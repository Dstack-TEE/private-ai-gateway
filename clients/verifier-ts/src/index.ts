/**
 * @dstack/aci-verifier — a zero-dependency ACI Level 1 verifier.
 *
 * Level 1 (receipt verification, §10.2) is fully implemented against an
 * established keyset. {@link verifyReportBinding} adds the cryptographic-binding
 * checks of Level 2 (§10.1 checks 2–6); the hardware quote, key custody, and
 * provenance checks (§10.1 checks 1, 7–10) are verifier-profile territory and
 * out of scope here. All crypto is Web Crypto (Ed25519, SHA-256); `ecdsa-secp256k1`
 * is unsupported (not in the Web Crypto API) and raises a clear error.
 */

// Canonicalization (§3)
export { canonicalize, jcsBytes } from './jcs.js';
export type { JcsValue } from './jcs.js';

// Crypto primitives (Web Crypto only)
export {
  sha256,
  sha256Hex,
  sha256Prefixed,
  verifyEd25519,
  verifySignature,
  toHex,
  fromHex,
} from './crypto.js';

// Digest & canonical-signing-bytes constructions (§4, §8.5, §9.2)
export {
  computeWorkloadId,
  computeKeysetDigest,
  attestationStatement,
  computeReportData,
  keysetEndorsementPayload,
  keysetRevocationPayload,
  receiptSigningBytes,
  sessionMaterial,
  computeSessionId,
} from './digest.js';

// E2EE AAD builders (§7.3)
export {
  requestAad,
  requestAadString,
  responseAad,
  responseAadString,
} from './e2ee.js';
export type { AadCommon } from './e2ee.js';

// Level 1 receipt verification (§10.2)
export {
  verifyReceipt,
  findEvent,
  hashBody,
  checkRequestBodyHash,
  checkResponseWireHash,
  checkResponseCleartextHash,
} from './receipt.js';

// Level 2 report-binding checks (§10.1 checks 2–6, no hardware quote)
export { verifyReportBinding } from './report.js';
export type { ReportBindingOptions } from './report.js';

// Errors
export { AciError, AciFormatError, UnsupportedAlgorithmError } from './errors.js';

// Wire & result types
export type {
  PublicKey,
  WorkloadIdentity,
  ReceiptSigningKey,
  WorkloadKeyset,
  ReceiptSignature,
  ReceiptEvent,
  Receipt,
  Endorsement,
  Attestation,
  AttestationReport,
  SessionEvidence,
  SessionRecord,
  Check,
  ReceiptVerification,
  ReportVerification,
} from './types.js';
