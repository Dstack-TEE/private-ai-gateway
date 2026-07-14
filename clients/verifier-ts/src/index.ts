/**
 * @phala/aci-verifier — a TypeScript ACI verifier for the browser and node.
 *
 * {@link verifyService} is the one call: fetch a service's attestation report
 * with a fresh nonce and get a full §10.1 transcript, including the hardware
 * quote (L2.1, verified with @phala/dcap-qvl against the Phala PCCS) and the
 * compose measurement (L2.4). Also exposes the individual checks: report
 * binding (§10.1 checks 2–3), receipts and body hashes (§10.2), sessions
 * (§9, §10.3), and the v3 sealed-body E2EE channel (§7). Every check other than
 * the quote is Web Crypto (Ed25519, X25519, HKDF, AES-GCM, SHA-256).
 */

// Crypto primitives (Web Crypto)
export {
  sha256,
  sha256Hex,
  sha256Prefixed,
  verifyEd25519,
  toHex,
  fromHex,
  toBase64,
  fromBase64,
} from './crypto.js';

// Digest constructions (§3, §4.1, §4.2)
export { computeKeysetDigest, attestationStatement, computeReportData } from './digest.js';

// Attested sessions: content addressing and evidence (§9, §10.3)
export { computeSessionId, checkSessionApiVersion, checkSessionEvidence } from './session.js';

// E2EE v3: sealed-body channel to a verified workload (§7)
export {
  E2EE_ALGORITHM,
  REQUEST_CONTEXT,
  RESPONSE_CONTEXT,
  e2eeAad,
  sealUnit,
  openUnit,
  openE2eeChannel,
} from './e2ee.js';
export type { E2eeContext, E2eeChannel, SealedRequest } from './e2ee.js';

// Receipt verification (§10.2)
export {
  verifyReceipt,
  findEvent,
  hashBody,
  checkRequestBodyHash,
  checkResponseBodyHash,
} from './receipt.js';

// Report binding (§10.1 checks 2–3), quote verification (check 1), compose
// measurement (check 4)
export { verifyReportBinding, verifyComposeMeasurement, verifyQuote } from './report.js';
export type { ReportBindingOptions } from './report.js';

// High-level transcript + one-call service verification
export { verifyService, reportTranscript, receiptTranscript, computeVerdict } from './transcript.js';
export type {
  CheckStatus,
  TranscriptLine,
  Verdict,
  ReportTranscript,
  ReceiptTranscript,
  TranscriptOptions,
  VerifyServiceOptions,
} from './transcript.js';

// Errors
export { AciError, AciFormatError } from './errors.js';

// Wire & result types
export type {
  KeysetKey,
  TlsKeyPin,
  WorkloadKeyset,
  SourceProvenance,
  Attestation,
  AttestationReport,
  ReceiptEnvelope,
  ReceiptEvent,
  ReceiptPayload,
  SessionEvidence,
  SessionRecord,
  Check,
  ReceiptVerification,
  ReportVerification,
} from './types.js';
