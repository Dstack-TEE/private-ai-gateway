/**
 * @phala/aci-verifier — a TypeScript ACI verifier for the browser and node.
 *
 * {@link verifyService} is the one call: fetch a service's attestation report
 * with a fresh nonce and get a full §9.1 transcript, including the hardware
 * quote (id-1, verified with @phala/dcap-qvl against the Phala PCCS) and the
 * compose measurement (id-4). Also exposes the individual checks: report
 * binding (§9.1 checks 2–3), receipts and body hashes (§9.3), sessions
 * (§8, §9.3), and the v2 sealed-body E2EE channel (§6). Every check other than
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
  jcsBytes,
} from './crypto.js';

// Digest constructions (Appendix A, §3.1, §3.2)
export { computeKeysetDigest, attestationStatement, computeReportData } from './digest.js';

// Attested sessions: content addressing and evidence (§8, §9.3)
export { computeSessionId, checkSessionApiVersion, checkSessionEvidence } from './session.js';

// Receipt verification (§9.3)
export {
  verifyReceipt,
  findEvent,
  hashBody,
  checkRequestBodyHash,
  checkResponseBodyHash,
} from './receipt.js';

// Report binding (§9.1 checks 2–3), quote verification (check 1), compose
// measurement (check 4)
export { verifyReportBinding, verifyComposeMeasurement, verifyQuote } from './report.js';
export type { ReportBindingOptions } from './report.js';

// High-level transcript + one-call service verification
export { verifyService, reportTranscript, receiptTranscript, computeVerdict } from './transcript.js';
export type { UpstreamAuditInput } from './transcript.js';
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
