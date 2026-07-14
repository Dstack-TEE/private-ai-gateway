/**
 * The verification transcript engine plus the one-call {@link verifyService}
 * entry point. DOM-free, so any web or node project imports it directly. Check
 * ids, titles, and section cites are the shared ACI transcript vocabulary (the
 * `aci` CLI prints the same lines); a check that cannot run is reported as
 * `skip` with the reason, never as a pass.
 */

import { verifyReportBinding, verifyComposeMeasurement, verifyQuote } from './report.js';
import { verifyReceipt, checkRequestBodyHash, checkResponseBodyHash } from './receipt.js';
import { AciError } from './errors.js';
import type {
  AttestationReport,
  ReceiptEnvelope,
  WorkloadKeyset,
  ReportVerification,
  Check,
} from './types.js';

export type CheckStatus = 'pass' | 'fail' | 'skip' | 'info';

export interface TranscriptLine {
  /** Shared check id, e.g. `L2.2` or `R.1`. */
  id: string;
  /** Spec section cite, e.g. `10.1(2)`. */
  section: string;
  title: string;
  status: CheckStatus;
  detail?: string;
  /** Short clause for the verdict line; set on `skip` lines. */
  reason?: string;
}

export interface Verdict {
  /** True when no check failed and the hardware root (L2.1) passed. */
  verified: boolean;
  /** One-line summary, e.g. `VERIFIED (4 pass, 2 skipped: …)`. */
  line: string;
}

export interface ReportTranscript {
  lines: TranscriptLine[];
  verdict: Verdict;
  verification: ReportVerification;
}

export interface TranscriptOptions {
  /** Fixed clock (Unix seconds) for deterministic runs; defaults to local. */
  now?: number;
  /** PCCS base URL for quote collateral; defaults to the Phala PCCS. */
  pccsUrl?: string;
}

const L2_TITLES: Record<string, [section: string, title: string]> = {
  'L2.1': ['10.1(1)', 'hardware quote verifies to the TEE vendor root and binds report_data'],
  'L2.2': ['10.1(2)', 'keyset bytes → digest → statement → report_data recomputed for our nonce'],
  'L2.3': ['10.1(3)', 'keyset not expired (now < not_after)'],
  'L2.4': ['10.1(4)', 'the running compose is measured into the quote (source provenance)'],
  'L2.5': ['10.1(5)', 'private-key custody satisfies the verifier profile'],
  'L2.6': ['10.1(6)', 'the channel actually used is bound to the attested keyset'],
};

const R_TITLES: Record<string, [section: string, title: string]> = {
  'R.1': ['10.2(1)', 'Ed25519 signature over the served payload bytes under an attested receipt key'],
  'R.2': ['10.2(2)', 'payload keyset digest equals the established digest'],
  'R.3': ['10.2(3)', 'request.received body hash matches the sent bytes'],
  'R.4': ['10.2(4)', 'response.returned body hash matches the received bytes'],
};

function line(
  titles: Record<string, [string, string]>,
  id: string,
  status: CheckStatus,
  detail?: string,
  reason?: string,
): TranscriptLine {
  const [section, title] = titles[id] ?? ['?', id];
  return { id, section, title, status, ...(detail ? { detail } : {}), ...(reason ? { reason } : {}) };
}

/** Verdict wording shared with the CLI: skips are counted and explained, never
 *  passed off, and VERIFIED requires the hardware root (L2.1) to have passed. */
export function computeVerdict(lines: TranscriptLine[]): Verdict {
  const pass = lines.filter((l) => l.status === 'pass').length;
  const fails = lines.filter((l) => l.status === 'fail');
  const skips = lines.filter((l) => l.status === 'skip');
  const skipClause = skips.length
    ? `, ${skips.length} skipped: ${skips.map((s) => s.reason ?? s.id).join(', ')}`
    : '';
  if (fails.length > 0) {
    return {
      verified: false,
      line: `NOT VERIFIED (${fails.length} fail: ${fails.map((f) => f.id).join(', ')}; ${pass} pass${skipClause})`,
    };
  }
  const l21 = lines.find((l) => l.id === 'L2.1');
  if (l21 && l21.status !== 'pass') {
    return { verified: false, line: `PARTIAL — hardware root not verified (${pass} pass${skipClause})` };
  }
  return { verified: true, line: `VERIFIED (${pass} pass${skipClause})` };
}

function libCheck(checks: Check[], name: string): Check | undefined {
  return checks.find((c) => c.name === name);
}

function unixDate(seconds: number): string {
  return new Date(seconds * 1000).toISOString().replace('.000Z', 'Z');
}

function randomNonceHex(): string {
  const b = new Uint8Array(32);
  crypto.getRandomValues(b);
  return Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}

export interface VerifyServiceOptions extends TranscriptOptions {
  /** Nonce to send; a fresh 32-byte random hex value by default. */
  nonce?: string;
  /** Fetch implementation; the global `fetch` by default. */
  fetchImpl?: typeof fetch;
}

/**
 * Fetch a service's attestation report with a fresh nonce and run the §10.1
 * transcript — one call for any web or node project. Verifies the quote (L2.1,
 * via @phala/dcap-qvl and the default Phala PCCS), the binding chain (L2.2/L2.3),
 * and the compose measurement (L2.4) when the service publishes `app_compose`.
 * Custody (L2.5) and the TLS pin (L2.6) stay out of a plain browser's reach.
 */
export async function verifyService(
  baseUrl: string,
  options: VerifyServiceOptions = {},
): Promise<ReportTranscript> {
  const nonce = options.nonce ?? randomNonceHex();
  const doFetch = options.fetchImpl ?? fetch;
  const url = `${baseUrl.replace(/\/+$/, '')}/v1/aci/attestation?nonce=${encodeURIComponent(nonce)}`;
  const res = await doFetch(url);
  if (!res.ok) {
    throw new AciError(`attestation fetch failed: HTTP ${res.status}`);
  }
  const report = (await res.json()) as AttestationReport;
  return reportTranscript(report, nonce, {
    ...(options.now !== undefined ? { now: options.now } : {}),
    ...(options.pccsUrl !== undefined ? { pccsUrl: options.pccsUrl } : {}),
  });
}

/**
 * Run the §10.1 checks against a fetched report and render the transcript.
 * `nonce` must be the value this client sent with the request.
 */
export async function reportTranscript(
  report: AttestationReport,
  nonce: string,
  options: TranscriptOptions = {},
): Promise<ReportTranscript> {
  const verification = await verifyReportBinding(report, nonce, {
    ...(options.now !== undefined ? { now: options.now } : {}),
  });
  const checks = verification.checks;
  const lines: TranscriptLine[] = [];

  // L2.1 — the hardware root: the quote verifies to the Intel vendor root and
  // binds report_data. This is what makes L2.4's RTMR3 authentic.
  const quote = await verifyQuote(report, options.pccsUrl);
  lines.push(
    quote.ok
      ? line(L2_TITLES, 'L2.1', 'pass', `TDX quote verified to the Intel root (TCB ${quote.status})`)
      : line(L2_TITLES, 'L2.1', 'fail', quote.detail ?? 'quote verification failed'),
  );

  // L2.2 — the full binding chain, including the aci/1 protocol gate.
  const bindingChecks = ['api_version', 'workload_keyset_digest', 'report_data'].map((name) =>
    libCheck(checks, name),
  );
  const failed = bindingChecks.find((c) => !c?.ok);
  lines.push(
    failed === undefined
      ? line(L2_TITLES, 'L2.2', 'pass', `${verification.workloadKeysetDigest} bound for our nonce`)
      : line(L2_TITLES, 'L2.2', 'fail', failed?.detail ?? 'binding recomputation failed'),
  );

  const expiry = libCheck(checks, 'not_after');
  const notAfter = verification.keyset?.not_after;
  lines.push(
    expiry?.ok
      ? line(
          L2_TITLES,
          'L2.3',
          'pass',
          typeof notAfter === 'number' ? `keyset valid until ${unixDate(notAfter)}` : undefined,
        )
      : line(L2_TITLES, 'L2.3', 'fail', expiry?.detail ?? 'expiry check did not run'),
  );

  // L2.4 — the running compose measured into RTMR3 (authentic once L2.1 passed).
  // A service that does not publish app_compose falls back to an honest skip.
  try {
    const compose = await verifyComposeMeasurement(report);
    const bad = compose.checks.find((c) => !c.ok);
    lines.push(
      compose.ok
        ? line(L2_TITLES, 'L2.4', 'pass', 'compose measured into RTMR3; sha256(app_compose) matches')
        : line(L2_TITLES, 'L2.4', 'fail', bad?.detail ?? 'compose measurement failed'),
    );
  } catch {
    const provenance = report.attestation.source_provenance;
    const repoUrl = typeof provenance?.repo_url === 'string' ? provenance.repo_url : null;
    const repoCommit = typeof provenance?.repo_commit === 'string' ? provenance.repo_commit : null;
    lines.push(
      repoUrl && repoCommit
        ? line(
            L2_TITLES,
            'L2.4',
            'skip',
            `service publishes no app_compose; provenance is presence-only: ${repoUrl} @ ${repoCommit}`,
            'no app_compose to measure',
          )
        : line(L2_TITLES, 'L2.4', 'fail', 'the report declares no source provenance (§5.1)'),
    );
  }

  lines.push(
    line(
      L2_TITLES,
      'L2.5',
      'skip',
      'the key-custody evidence (§4.3) is verifier-profile territory — the aci CLI checks it',
      'custody profile check runs in the aci CLI',
    ),
  );

  lines.push(
    line(
      L2_TITLES,
      'L2.6',
      'skip',
      'browsers cannot observe the server certificate, so the TLS SPKI pin is uncheckable here; without E2EE this browser channel has WebPKI assurance only (spec §1.1) — use `aci verify` or the `aci serve` proxy for a pinned channel',
      'TLS pin not observable in a browser',
    ),
  );

  return { lines, verdict: computeVerdict(lines), verification };
}

export interface ReceiptTranscript {
  lines: TranscriptLine[];
  verdict: Verdict;
}

/**
 * Run the §10.2 checks against a receipt envelope and the keyset the report
 * verification established. Byte inputs are optional: absent bytes make R.3/R.4
 * skips, not passes.
 */
export async function receiptTranscript(
  envelope: ReceiptEnvelope,
  keyset: WorkloadKeyset,
  establishedDigest: string,
  requestBytes?: Uint8Array | string,
  responseBytes?: Uint8Array | string,
): Promise<ReceiptTranscript> {
  const result = await verifyReceipt(envelope, keyset, establishedDigest);
  const lines: TranscriptLine[] = [];

  const sig = libCheck(result.checks, 'signature');
  lines.push(
    sig?.ok
      ? line(R_TITLES, 'R.1', 'pass', `key "${envelope.key_id}" (${envelope.algo})`)
      : line(R_TITLES, 'R.1', 'fail', sig?.detail ?? 'signature verification failed'),
  );

  const digest = libCheck(result.checks, 'workload_keyset_digest');
  lines.push(
    digest?.ok
      ? line(R_TITLES, 'R.2', 'pass', 'the payload binds to the verified keyset')
      : line(R_TITLES, 'R.2', 'fail', digest?.detail ?? 'binding mismatch'),
  );

  if (result.payload === undefined || requestBytes === undefined) {
    lines.push(line(R_TITLES, 'R.3', 'skip', 'request bytes not supplied', 'request bytes not supplied'));
  } else {
    const ok = await checkRequestBodyHash(result.payload, requestBytes);
    lines.push(
      line(R_TITLES, 'R.3', ok ? 'pass' : 'fail', ok ? undefined : 'request.received.body_hash does not match the supplied bytes'),
    );
  }

  if (result.payload === undefined || responseBytes === undefined) {
    lines.push(line(R_TITLES, 'R.4', 'skip', 'response bytes not supplied', 'response bytes not supplied'));
  } else {
    const ok = await checkResponseBodyHash(result.payload, responseBytes);
    lines.push(
      line(R_TITLES, 'R.4', ok ? 'pass' : 'fail', ok ? undefined : 'response.returned.body_hash does not match the supplied bytes'),
    );
  }

  return { lines, verdict: computeVerdict(lines) };
}
