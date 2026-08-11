// In-memory cache for the last validated attestation report. Used to source
// the attested TLS SPKI pin (prevention). There is no per-response receipt
// tracking here: pinning is the security control, and this module exists only
// to hold the validated report between session start and pin install.

import type { AciCloudConfig } from "./config.ts";
import {
  ATTESTATION_FALLBACK_TTL_MS,
  LOG_PREFIX,
} from "./constants.ts";
import {
  type AttestationReport,
  type ReportVerification,
  type WorkloadKeyset,
  bindAttestation,
  fetchAttestation,
  keysetStaleAfterMs,
  newNonce,
} from "./verify.ts";

export interface Attested {
  report: AttestationReport;
  verification: ReportVerification;
  fetchedAt: number;
}

export class AciAttestationStore {
  private _lastAttestationError?: string;
  private cachedAttestation?: Attested;

  /** Validated binding for the cached attestation, if present. */
  get binding(): ReportVerification | undefined {
    return this.cachedAttestation?.verification;
  }

  /** Keyset established by the cached binding, if any. */
  get establishedKeyset(): WorkloadKeyset | undefined {
    return this.cachedAttestation?.verification.keyset;
  }

  get lastAttestationError(): string | undefined {
    return this._lastAttestationError;
  }

  /** Fetch + validate the attestation report. Returns the validated artifact,
   *  or null (with lastAttestationError set) when the fetch or binding fails. */
  async getAttestation(apiKey: string, config: AciCloudConfig): Promise<Attested | null> {
    const now = Date.now();
    if (this.cachedAttestation && this.isFresh(this.cachedAttestation, now)) {
      return this.cachedAttestation;
    }

    let lastError: string | undefined;
    for (let attempt = 0; attempt < 2; attempt++) {
      const nonce = newNonce();
      const report = await fetchAttestation(apiKey, nonce, { baseUrl: config.baseUrl });
      if (!report) return null;
      try {
        const verification = await bindAttestation(report, nonce);
        if (verification) {
          this._lastAttestationError = undefined;
          this.cachedAttestation = { report, verification, fetchedAt: Date.now() };
          return this.cachedAttestation;
        }
        lastError = "report binding failed (see console)";
      } catch (error) {
        lastError = error instanceof Error ? error.message : String(error);
        console.error(
          `${LOG_PREFIX} attestation report binding failed (attempt ${attempt + 1}):`,
          error,
        );
      }
    }
    this._lastAttestationError = lastError;
    return null;
  }

  private isFresh(cached: Attested, now: number): boolean {
    const staleAfterMs = keysetStaleAfterMs(cached.verification.keyset);
    if (typeof staleAfterMs === "number") return staleAfterMs > now;
    return now - cached.fetchedAt < ATTESTATION_FALLBACK_TTL_MS;
  }

  reset(): void {
    this.cachedAttestation = undefined;
    this._lastAttestationError = undefined;
  }
}