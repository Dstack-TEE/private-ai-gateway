import { AciError } from '../errors.js';
import type { AttestationReport, ReceiptEnvelope, WorkloadKeyset } from '../types.js';
import type { ReceiptTranscript, ReportTranscript } from '../transcript.js';

export type AciConnectionErrorCode =
  | 'invalid_base_url'
  | 'invalid_policy'
  | 'invalid_request_url'
  | 'origin_mismatch'
  | 'attestation_fetch'
  | 'attestation_verification'
  | 'invalid_tls_pin'
  | 'channel_binding'
  | 'identity_expired'
  | 'invalid_serving_constraints'
  | 'receipt_missing'
  | 'receipt_not_found'
  | 'receipt_fetch'
  | 'receipt_verification'
  | 'closed';

export class AciConnectionError extends AciError {
  constructor(
    public readonly code: AciConnectionErrorCode,
    message: string,
    options?: { cause?: unknown },
  ) {
    super(message);
    this.name = 'AciConnectionError';
    if (options && 'cause' in options) this.cause = options.cause;
  }
}

export interface AciPolicy {
  /** Appraise the measured dstack OS hash against the production allowlist. */
  requireProductionOs?: boolean;
  /**
   * Reviewed RTMR3-bound compose hashes accepted by this client. Empty or
   * omitted means hardware-bound measurement without release pinning.
   */
  acceptedComposeHashes?: readonly string[];
}

export interface AciServingPolicy {
  /** Demand provider.aci_verified on JSON POST requests. Default: true. */
  requireVerified?: boolean;
  /** Require a receipt id on successful POST responses. Default: true. */
  requireReceipt?: boolean;
  /** Attested-session ids this connection accepts for verified serving. */
  acceptedSessionIds?: readonly string[];
}

export type AciFetch = (input: RequestInfo | URL, init?: RequestInit) => Promise<Response>;

export interface ConnectAciOptions {
  baseURL: string;
  policy?: AciPolicy;
  /** Gateway serving constraints applied to JSON POST bodies. */
  serving?: AciServingPolicy;
  /** Attestation/PCCS request timeout. Default: 10 seconds. */
  timeoutMs?: number;
  pccsUrl?: string;
  /** Fetch used only to bootstrap attestation over normal CA-validated TLS. */
  bootstrapFetch?: AciFetch;
  /** Explicit HTTP CONNECT proxy URL for the pinned inference transport. */
  proxy?: string;
  /** Additional CA certificate for private gateway deployments. */
  ca?: string | Buffer;
  /** Number of receipt-bearing POST exchanges retained for audit. Default: 32. */
  receiptHistorySize?: number;
}

export interface VerifiedAciIdentity {
  origin: string;
  hostname: string;
  report: AttestationReport;
  keyset: WorkloadKeyset;
  workloadKeysetDigest: string;
  /** RTMR3-bound sha256(app_compose). */
  composeHash: string;
  tlsSpkiPins: readonly string[];
  verifiedAt: number;
  expiresAt: number;
  transcript: ReportTranscript;
}

export interface RecordedAciExchange {
  receiptId: string;
  method: string;
  path: string;
  status: number;
  recordedAt: number;
  responseComplete: boolean;
  responseError?: string;
}

export interface AciReceiptAudit {
  receiptId: string;
  receipt: ReceiptEnvelope;
  transcript: ReceiptTranscript;
  exchange: RecordedAciExchange;
}

export interface AciConnection {
  readonly baseURL: string;
  readonly identity: VerifiedAciIdentity;
  readonly fetch: AciFetch;
  receipts(): readonly RecordedAciExchange[];
  /** Verify a recorded exchange's receipt, or the latest one when omitted. */
  verifyReceipt(receiptId?: string): Promise<AciReceiptAudit>;
  refresh(): Promise<void>;
  close(): Promise<void>;
}
