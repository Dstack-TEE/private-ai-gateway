import { AciError } from '../errors.js';
import type { AttestationReport, WorkloadKeyset } from '../types.js';
import type { ReportTranscript } from '../transcript.js';

export type AciConnectionErrorCode =
  | 'invalid_base_url'
  | 'invalid_request_url'
  | 'origin_mismatch'
  | 'attestation_fetch'
  | 'attestation_verification'
  | 'invalid_tls_pin'
  | 'channel_binding'
  | 'identity_expired'
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
  /** Require source provenance to be backed by a compose measurement. Default: true. */
  requireComposeMeasurement?: boolean;
  /** Appraise the measured dstack OS hash against the production allowlist. */
  requireProductionOs?: boolean;
  /** Exact source claims accepted by the caller. Every supplied field must match. */
  expectedSource?: {
    repoUrl?: string;
    repoCommit?: string;
    imageDigest?: string;
  };
}

export interface ConnectAciOptions {
  baseURL: string;
  apiKey?: string;
  policy?: AciPolicy;
  /** Attestation/PCCS request timeout. Default: 10 seconds. */
  timeoutMs?: number;
  pccsUrl?: string;
  /** Fetch used only to bootstrap attestation over normal CA-validated TLS. */
  bootstrapFetch?: typeof globalThis.fetch;
  /** Explicit HTTP CONNECT proxy URL for the pinned inference transport. */
  proxy?: string;
  /** Additional CA certificate for private gateway deployments. */
  ca?: string | Buffer;
}

export interface VerifiedAciIdentity {
  origin: string;
  hostname: string;
  report: AttestationReport;
  keyset: WorkloadKeyset;
  workloadKeysetDigest: string;
  tlsSpkiSha256: string;
  verifiedAt: number;
  expiresAt: number;
  transcript: ReportTranscript;
}

export interface AciConnection {
  readonly baseURL: string;
  readonly identity: VerifiedAciIdentity;
  readonly fetch: typeof globalThis.fetch;
  refresh(): Promise<void>;
  close(): Promise<void>;
}
