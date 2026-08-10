// In-memory store for the last response's receipt metadata, a cached attestation
// report (with freshness), and the verified classification. The user-facing
// verification status (statusText) is derived from this store.

import type { AciCloudConfig } from "./config.ts";
import {
  ATTESTATION_FALLBACK_TTL_MS,
  HEADER_ACI_IDENTITY,
  HEADER_ACI_KEYSET_DIGEST,
  HEADER_RECEIPT_ID,
  LOG_PREFIX,
  PROVIDER_ID,
} from "./constants.ts";
import {
  type AttestationReport,
  type ReceiptEnvelope,
  type ReportVerification,
  type WorkloadKeyset,
  bindAttestation,
  classifyReceipt,
  fetchAttestation,
  fetchReceipt,
  isFullyVerified,
  keysetStaleAfterMs,
  newNonce,
} from "./verify.ts";
import type { ReceiptClassification } from "./verify.ts";

export interface ResponseHeaderSnapshot {
  receiptId?: string;
  aciIdentity?: string;
  keysetDigest?: string;
}

export interface Attested {
  report: AttestationReport;
  verification: ReportVerification;
  fetchedAt: number;
}

export class AciReceiptStore {
  private lastReceiptId?: string;
  private lastAciIdentity?: string;
  private lastKeysetDigest?: string;
  private lastClassification?: ReceiptClassification;
  private _lastAttestationError?: string;
  private cachedAttestation?: Attested;
  private lastRequestBody?: Uint8Array;
  private lastResponseBytes?: Uint8Array;

  recordResponseHeaders(headers: Record<string, string>): ResponseHeaderSnapshot {
    const lower = lowerHeaders(headers);
    this.lastReceiptId = lower[HEADER_RECEIPT_ID] ?? lower["x-receipt-id"];
    this.lastAciIdentity = lower[HEADER_ACI_IDENTITY] ?? lower["x-aci-identity"];
    this.lastKeysetDigest = lower[HEADER_ACI_KEYSET_DIGEST] ?? lower["x-aci-keyset-digest"];
    this.lastClassification = undefined;
    return this.snapshot();
  }

  snapshot(): ResponseHeaderSnapshot {
    return {
      receiptId: this.lastReceiptId,
      aciIdentity: this.lastAciIdentity,
      keysetDigest: this.lastKeysetDigest,
    };
  }

  get classification(): ReceiptClassification | undefined {
    return this.lastClassification;
  }

  /** Stash the last request body bytes for request.received.body_hash verification. */
  setLastRequestBody(bytes: Uint8Array): void {
    this.lastRequestBody = bytes;
  }

  /** Stash the last response bytes for response.returned.body_hash verification. */
  setLastResponseBytes(bytes: Uint8Array): void {
    this.lastResponseBytes = bytes;
  }

  get lastAttestationError(): string | undefined {
    return this._lastAttestationError;
  }

  /** Validated binding for the cached attestation, if present. */
  get binding(): ReportVerification | undefined {
    return this.cachedAttestation?.verification;
  }

  /** Keyset established by the cached binding, if any. */
  get establishedKeyset(): WorkloadKeyset | undefined {
    return this.cachedAttestation?.verification.keyset;
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

  /** Fetch the receipt for the last response and run full verification against a
   *  cached attestation. Stores the classification for footer rendering.
   *
   *  Fails closed: when `config.verify.requireAttestationMatch` is set and no
   *  validated attestation with a matching keyset is available, the
   *  classification is reported as mismatch/unverified rather than a silent
   *  semantic-only pass. */
  async classifyLastResponse(
    apiKey: string,
    config: AciCloudConfig,
    _options: { requestBody?: Uint8Array; responseBytes?: Uint8Array } = {},
  ): Promise<ReceiptClassification | null> {
    if (!this.lastReceiptId) return null;
    // Capture the receipt this call is for so a slower, earlier in-flight
    // classification cannot overwrite the newer one when it finally resolves
    // (message_end fires per response; they resolve out of order).
    const targetReceiptId = this.lastReceiptId;

    const receipt: ReceiptEnvelope | null = await fetchReceipt(apiKey, targetReceiptId, {
      baseUrl: config.baseUrl,
    });
    if (!receipt) return null;

    const attested = await this.getAttestation(apiKey, config);
    const keyset = attested?.verification.keyset;
    const digest = attested?.verification.workloadKeysetDigest;
    const requireMatch = config.verify.requireAttestationMatch;

    if (attested && keyset && digest) {
      const classification = await classifyReceipt(receipt, keyset, digest, {
        requestBody: this.lastRequestBody ?? _options.requestBody,
        responseBytes: this.lastResponseBytes ?? _options.responseBytes,
      });
      // Only publish the classification if this is still the latest receipt.
      if (this.lastReceiptId === targetReceiptId) {
        this.lastClassification = classification;
      }
      return classification;
    }

    // No valid attestation. requireAttestationMatch means the user asked for
    // responses to be gated on a matching attested workload — a receipt we
    // cannot bind to one must not render "verified".
    if (requireMatch) {
      const blocked: ReceiptClassification = {
        status: "unknown",
        signatureValid: false,
        hashesChecked: false,
        hashesNotCheckedReason: this.lastAttestationError ?? "no valid attestation available",
      };
      if (this.lastReceiptId === targetReceiptId) this.lastClassification = blocked;
      return blocked;
    }

    // Best-effort semantic classification without a binding (footers: routed /
    // attested / mismatch — never "verified"). This mirrors the previous
    // no-attestation fallback, minus any claim of full verification.
    const basic: ReceiptClassification = { status: "routed", hashesChecked: false };
    if (this.lastReceiptId === targetReceiptId) this.lastClassification = basic;
    return basic;
  }

  reset(): void {
    this.lastReceiptId = undefined;
    this.lastAciIdentity = undefined;
    this.lastKeysetDigest = undefined;
    this.lastClassification = undefined;
    this.cachedAttestation = undefined;
    this._lastAttestationError = undefined;
    this.lastRequestBody = undefined;
    this.lastResponseBytes = undefined;
  }
}

// Verification status text. No emoji per project conventions.
//   signature+workload+hashes verified            -> "<id>: verified"
//   receipt verified but signature/hash not checked -> "<id>: verified*"
//   routed (upstream not attested)                -> "<id>: routed"
//   signature FAILED or keyset mismatch           -> "<id>: mismatch"
//   attestation/report pending                     -> "<id>: attested"
//   no receipt header                              -> "<id>: (no receipt)"
export function statusText(store: AciReceiptStore): string {
  const classification = store.classification;

  if (classification) {
    if (isFullyVerified(classification)) return `${PROVIDER_ID}: verified`;
    if (classification.signatureValid === false) return `${PROVIDER_ID}: mismatch`;
    if (classification.status === "routed") return `${PROVIDER_ID}: routed`;
    if (classification.signatureValid === true && !classification.hashesChecked) {
      return `${PROVIDER_ID}: verified*`;
    }
    if (classification.status === "verified") return `${PROVIDER_ID}: verified*`;
    return `${PROVIDER_ID}: attested`;
  }

  if (store.snapshot().receiptId || store.snapshot().aciIdentity) {
    return `${PROVIDER_ID}: attested`;
  }
  return `${PROVIDER_ID}: (no receipt)`;
}

function lowerHeaders(headers: Record<string, string>): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [key, value] of Object.entries(headers)) {
    out[key.toLowerCase()] = value;
  }
  return out;
}
