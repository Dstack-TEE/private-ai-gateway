/**
 * The verification transcript engine plus the one-call {@link verifyService}
 * entry point. DOM-free, so any web or node project imports it directly. Check
 * ids, titles, and section cites are the shared ACI transcript vocabulary (the
 * `aci` CLI prints the same lines); a check that cannot run is reported as
 * `skip` with the reason, never as a pass.
 */

import { verifyReportBinding, verifyComposeMeasurement, verifyQuote } from './report.js';
import { verifyReceipt, checkRequestBodyHash, checkResponseBodyHash, findEvent } from './receipt.js';
import { computeSessionId, checkSessionApiVersion, checkSessionEvidence } from './session.js';
import { AciError } from './errors.js';
import type {
  AttestationReport,
  ReceiptEnvelope,
  ReceiptPayload,
  SessionRecord,
  WorkloadKeyset,
  ReportVerification,
  Check,
} from './types.js';

export type CheckStatus = 'pass' | 'fail' | 'skip' | 'info';

export interface TranscriptLine {
  /** Shared check id, e.g. `id-2` or `receipt-1`. */
  id: string;
  /** Spec section cite, e.g. `9.1(2)`. */
  section: string;
  title: string;
  status: CheckStatus;
  detail?: string;
  /** Short clause for the verdict line; set on `skip` lines. */
  reason?: string;
}

export interface Verdict {
  /** True when no check failed and the hardware root (id-1) passed. */
  verified: boolean;
  /** One-line summary, e.g. `VERIFIED (4 pass, 2 skipped: …)`. */
  line: string;
}

export interface ReportTranscript {
  lines: TranscriptLine[];
  verdict: Verdict;
  verification: ReportVerification;
  /** RTMR3-bound sha256(app_compose), present only when id-4 measured successfully. */
  composeHash?: string;
}

export interface TranscriptOptions {
  /** Fixed clock (Unix seconds) for deterministic runs; defaults to local. */
  now?: number;
  /** PCCS base URL for quote collateral; defaults to the Phala PCCS. */
  pccsUrl?: string;
  /**
   * Live run against the service (`verifyService` sets this). Online, a
   * provenance claim no measurement backs fails id-4 (§9.1(4)) and an
   * unbound channel fails id-6 (§1.1); offline (auditing a stored report)
   * both stay honest skips.
   */
  online?: boolean;
  /**
   * How this client's channel is bound to the attested keyset (§9.1(6)):
   * the TLS leaf SPKI the caller's own stack observed. A browser cannot
   * observe TLS, so its channel is unbound — use the `aci` CLI or the
   * `aci serve` proxy for a pinned channel.
   */
  channel?: { observedSpkiSha256: string; host?: string };
  /**
   * Appraise the RTMR3 `os-image-hash` against this release's production
   * allowlist (§1.3). This does not reconstruct MRTD/RTMR0-2. Use it only
   * after a dstack verifier binds the same evidence to the OS-image hash.
   */
  requireProductionOs?: boolean;
  /**
   * Reviewed RTMR3-bound compose hashes accepted by this verifier (§1.3).
   * An empty or omitted list verifies and reports the measurement without
   * claiming that the release was reviewed.
   */
  acceptedComposeHashes?: readonly string[];
}

const DSTACK_RUNTIME_EVENT_TYPE = 0x08000001;
interface DstackEvent {
  imr: number;
  event_type: number;
  digest: string;
  event: string;
  event_payload: string;
}

// Source: the `is_dev: false` entries returned by `phala os-images --json` on
// 2026-08-13 (dstack 0.5.8–0.5.9, standard and NVIDIA). The OS hashes were also
// checked with `uv run python scripts/dstack_os_image.py <os-image-hash>`.
//
// Each value is that image's measured dstack v1 runtime-event digest:
// SHA384(u32le(0x08000001) || ":os-image-hash:" || hex(os-image-hash)).
// Upstream definition:
// https://github.com/Dstack-TEE/dstack/blob/5d7d966e2d7ec179c6cf6794f2d214dd54661ef9/dstack/cc-eventlog/src/runtime_events.rs#L113-L139
const PRODUCTION_DSTACK_OS_IMAGES = new Map([
  // dstack-0.5.9 (standard)
  [
    'bd369a8c2f9edb2b52dad48ac8e0b32dde5f1337c423a506b48d07403a7d8033',
    'c936f709860155a455c519b505fe29bde21d1b778534a8f0b41b706b0f789eae2ea899e224fedb2331c18324aab4db93',
  ],
  // dstack-nvidia-0.5.9
  [
    '806a352e16175d90568de97dff563f31f680239e6b90e9b5b2e9141d0955b0d9',
    '87cd7e9dd097fa5ccdadf01c77ae4d86eb69debae40c7c9ecd2b0e0ff8ddb37ceeafd3607a0f49c7cfb81eeeaffa17c1',
  ],
  // dstack-0.5.8 (standard)
  [
    '6427f4f5ded88b72d326bd973e581c1689c5080c6444a0cf90fec7d9e4c8b92a',
    'dc15768ace33ac7ecd2f86dc032917ed0674d8218a8e138db68e16a4985b2fc4c4576a4b2c8697b83d2a88c990b6f855',
  ],
  // dstack-nvidia-0.5.8
  [
    '5d286131b20689d0d59bae1b34cd91cb78149016a9ef50afff51fb1a04cad5e4',
    '040e24a4f644add401d92cedf588eef268bce2089298feac2e54a0f63af63fef47028c2915af65b61ab6161294251614',
  ],
]);

function parseDstackEvents(report: AttestationReport): DstackEvent[] {
  const raw = (report.attestation.evidence as Record<string, unknown> | undefined)?.event_log;
  if (typeof raw !== 'string') throw new Error('attestation evidence carries no dstack event_log');
  const values = JSON.parse(raw) as unknown;
  if (!Array.isArray(values)) throw new Error('dstack event_log is not an array');

  return values.map((value, index) => {
    if (value === null || typeof value !== 'object') {
      throw new Error(`dstack event_log entry ${index} is not an object`);
    }
    const { imr, event_type, digest, event, event_payload } = value as Record<string, unknown>;
    if (
      typeof imr !== 'number' ||
      typeof event_type !== 'number' ||
      typeof digest !== 'string' ||
      typeof event !== 'string' ||
      typeof event_payload !== 'string'
    ) {
      throw new Error(`dstack event_log entry ${index} has invalid fields`);
    }
    return { imr, event_type, digest, event, event_payload };
  });
}

function productionDstackOs(report: AttestationReport): string {
  let measurement: { hash: string; digest: string } | undefined;
  for (const event of parseDstackEvents(report)) {
    if (event.imr !== 3 || event.event_type !== DSTACK_RUNTIME_EVENT_TYPE) continue;
    if (event.event === 'system-ready') break;
    if (event.event !== 'os-image-hash') continue;
    if (measurement !== undefined) {
      throw new Error('verified event log carries multiple os-image-hash events');
    }
    measurement = {
      hash: event.event_payload.toLowerCase(),
      digest: event.digest.toLowerCase(),
    };
  }
  if (measurement === undefined) throw new Error('verified event log carries no os-image-hash event');

  const { hash, digest } = measurement;
  if (PRODUCTION_DSTACK_OS_IMAGES.get(hash) !== digest) {
    throw new Error(`OS-image hash ${hash} is not a reviewed production image`);
  }
  return hash;
}

const ID_TITLES: Record<string, [section: string, title: string]> = {
  'id-1': ['9.1(1)', 'hardware quote verifies to the TEE vendor root and binds report_data'],
  'id-2': ['9.1(2)', 'keyset JCS → digest → statement → report_data recomputed for our nonce'],
  'id-3': ['9.1(3)', 'keyset not expired (now < not_after)'],
  'id-4': ['9.1(4)', 'the running compose is measured into the quote (source provenance)'],
  'id-5': ['9.1(5)', 'private-key custody satisfies the verifier policy'],
  'id-6': ['9.1(6)', 'the channel actually used is bound to the attested keyset'],
  'policy-os': ['1.3', 'RTMR3 os-image-hash satisfies the production allowlist'],
};

const UPSTREAM_TITLES: Record<string, [section: string, title: string]> = {
  'upstream-1': ['9.3(5)', 'upstream.verified reports a verified upstream and cites a session'],
  'upstream-2': ['9.2(1-2), 9.3(6)', 'cited session: document hashes to the id, served_at in window, evidence digest'],
};

const RECEIPT_TITLES: Record<string, [section: string, title: string]> = {
  'receipt-1': ['9.3(1)', 'signature over JCS(document minus signature) under an attested receipt key'],
  'receipt-2': ['9.3(2)', 'document keyset digest equals the established digest'],
  'receipt-3': ['9.3(3)', 'request.received body hash matches the sent bytes'],
  'receipt-4': ['9.3(4)', 'response.returned body hash matches the received bytes'],
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
 *  passed off, and VERIFIED requires the hardware root (id-1) to have passed. */
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
  const l21 = lines.find((l) => l.id === 'id-1');
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
 * Fetch a service's attestation report with a fresh nonce and run the §9.1
 * transcript — one call for any web or node project. Verifies the quote (id-1,
 * via @phala/dcap-qvl and the default Phala PCCS), the binding chain (id-2/id-3),
 * and the compose measurement (id-4) when the service publishes `app_compose`.
 * Custody (id-5) and the TLS pin (id-6) stay out of a plain browser's reach.
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
    ...(options.channel !== undefined ? { channel: options.channel } : {}),
    ...(options.requireProductionOs !== undefined
      ? { requireProductionOs: options.requireProductionOs }
      : {}),
    ...(options.acceptedComposeHashes !== undefined
      ? { acceptedComposeHashes: options.acceptedComposeHashes }
      : {}),
    online: true,
  });
}

/**
 * Run the §9.1 checks against a fetched report and render the transcript.
 * `nonce` must be the value this client sent with the request.
 */
export async function reportTranscript(
  report: AttestationReport,
  nonce: string | null,
  options: TranscriptOptions = {},
): Promise<ReportTranscript> {
  const verification = await verifyReportBinding(report, nonce, {
    ...(options.now !== undefined ? { now: options.now } : {}),
  });
  const checks = verification.checks;
  const lines: TranscriptLine[] = [];

  // id-1 — the hardware root: the quote verifies to the Intel vendor root and
  // binds report_data. This is what makes id-4's RTMR3 authentic.
  const quote = await verifyQuote(report, options.pccsUrl);
  lines.push(
    quote.ok
      ? line(ID_TITLES, 'id-1', 'pass', `TDX quote verified to the Intel root (TCB ${quote.status})`)
      : line(ID_TITLES, 'id-1', 'fail', quote.detail ?? 'quote verification failed'),
  );

  // id-2 — the full binding chain, including the aci/1 protocol gate.
  const bindingChecks = ['api_version', 'workload_keyset_digest', 'report_data'].map((name) =>
    libCheck(checks, name),
  );
  const failed = bindingChecks.find((c) => !c?.ok);
  lines.push(
    failed !== undefined
      ? line(ID_TITLES, 'id-2', 'fail', failed?.detail ?? 'binding recomputation failed')
      : nonce === null
        ? line(
            ID_TITLES,
            'id-2',
            'skip',
            `${verification.workloadKeysetDigest} bound for the null statement; freshness needs a caller nonce (§9.1(2))`,
            'binding shown, freshness not established',
          )
        : line(ID_TITLES, 'id-2', 'pass', `${verification.workloadKeysetDigest} bound for our nonce`),
  );

  const expiry = libCheck(checks, 'not_after');
  const notAfter = verification.keyset?.not_after;
  lines.push(
    expiry?.ok
      ? line(
          ID_TITLES,
          'id-3',
          'pass',
          typeof notAfter === 'number' ? `keyset valid until ${unixDate(notAfter)}` : undefined,
        )
      : line(ID_TITLES, 'id-3', 'fail', expiry?.detail ?? 'expiry check did not run'),
  );

  // id-4 — the running compose measured into RTMR3 (authentic once id-1 passed).
  // §4.1: a verifier MUST reject a report without acceptable provenance, and
  // §9.1(4): a claim no measurement backs must not satisfy this check. Only a
  // service that publishes no app_compose at all gets the honest skip; once
  // one is published, any failure to verify it — including malformed
  // evidence — fails the check.
  const provenance = report.attestation.source_provenance;
  const repoUrl = typeof provenance?.repo_url === 'string' ? provenance.repo_url : null;
  const repoCommit = typeof provenance?.repo_commit === 'string' ? provenance.repo_commit : null;
  const imageDigest = typeof provenance?.image_digest === 'string' ? provenance.image_digest : null;
  // §4.1 accepts either repo_url + repo_commit, or image_digest.
  const declared = repoUrl && repoCommit ? `${repoUrl} @ ${repoCommit}` : (imageDigest ?? null);
  const publishesCompose =
    typeof (report.attestation.evidence as Record<string, unknown> | undefined)?.app_compose ===
    'string';
  let composeVerified = false;
  let composeHash: string | undefined;
  if (!declared) {
    lines.push(line(ID_TITLES, 'id-4', 'fail', 'the report declares no source provenance (§4.1)'));
  } else if (!publishesCompose) {
    lines.push(
      options.online
        ? line(
            ID_TITLES,
            'id-4',
            'fail',
            `no measurement backs the declared provenance (${declared}): the service publishes no app_compose (§9.1(4), §4.1)`,
          )
        : line(
            ID_TITLES,
            'id-4',
            'skip',
            `service publishes no app_compose; provenance is presence-only: ${declared}`,
            'no app_compose to measure',
          ),
    );
  } else {
    try {
      const compose = await verifyComposeMeasurement(report);
      composeVerified = compose.ok;
      composeHash = compose.composeHash;
      const bad = compose.checks.find((c) => !c.ok);
      const accepted = options.acceptedComposeHashes ?? [];
      const releaseAccepted =
        composeHash !== undefined &&
        (accepted.length === 0 || accepted.some((hash) => hash.toLowerCase() === composeHash));
      if (!compose.ok) {
        lines.push(line(ID_TITLES, 'id-4', 'fail', bad?.detail ?? 'compose measurement failed'));
      } else if (!releaseAccepted) {
        lines.push(
          line(
            ID_TITLES,
            'id-4',
            'fail',
            `measured compose-hash=${composeHash} is not in the accepted list`,
          ),
        );
      } else {
        lines.push(
          line(
            ID_TITLES,
            'id-4',
            'pass',
            accepted.length > 0
              ? `compose-hash=${composeHash} measured into RTMR3 and accepted by policy`
              : `compose-hash=${composeHash} measured into RTMR3 (not pinned by policy)`,
          ),
        );
      }
    } catch (e) {
      lines.push(
        line(
          ID_TITLES,
          'id-4',
          'fail',
          `published app_compose could not be verified: ${e instanceof Error ? e.message : String(e)}`,
        ),
      );
    }
  }

  if (options.requireProductionOs) {
    if (!composeVerified) {
      lines.push(
        line(
          ID_TITLES,
          'policy-os',
          'fail',
          'production OS allowlist cannot be evaluated until RTMR3 verifies',
        ),
      );
    } else {
      try {
        const hash = productionDstackOs(report);
        lines.push(
          line(
            ID_TITLES,
            'policy-os',
            'pass',
            `RTMR3 os-image-hash ${hash} is in the reviewed production allowlist`,
          ),
        );
      } catch (error) {
        lines.push(
          line(
            ID_TITLES,
            'policy-os',
            'fail',
            `RTMR3 os-image-hash did not satisfy the production allowlist: ${error instanceof Error ? error.message : String(error)}`,
          ),
        );
      }
    }
  }

  lines.push(
    line(
      ID_TITLES,
      'id-5',
      'skip',
      'key-custody appraisal (§3.3) is not implemented in the in-tree verifiers (conformance gaps item 1)',
      'custody policy not implemented',
    ),
  );

  lines.push(channelLine(report, verification.keyset, options));

  return {
    lines,
    verdict: computeVerdict(lines),
    verification,
    ...(composeHash === undefined ? {} : { composeHash }),
  };
}

/**
 * id-6 — the channel actually used (§9.1(6)): a TLS SPKI the caller's own
 * stack observed, matched against the attested keyset. Online with none,
 * the channel is unbound (§1.1) and the check fails; offline it is a skip.
 */
function channelLine(
  report: AttestationReport,
  keyset: WorkloadKeyset | undefined,
  options: TranscriptOptions,
): TranscriptLine {
  const channel = options.channel;
  if (channel !== undefined) {
    if (!keyset) {
      return line(ID_TITLES, 'id-6', 'fail', 'no decoded keyset to match the channel against (see id-2)');
    }
    const host = channel.host?.toLowerCase().replace(/\.$/, '');
    const observed = channel.observedSpkiSha256.toLowerCase();
    const entries = Array.isArray(keyset.tls_public_keys) ? keyset.tls_public_keys : [];
    // §3.1: a domain-scoped entry binds one hostname; an unscoped entry stays
    // a candidate for any host.
    const candidates = entries.filter(
      (k) => k.domain === undefined || host === undefined || k.domain.toLowerCase().replace(/\.$/, '') === host,
    );
    if (entries.length === 0) {
      return line(
        ID_TITLES,
        'id-6',
        'fail',
        'keyset publishes no TLS role: pin nothing, or encrypt through E2EE instead (§1.1, §9.1(6))',
      );
    }
    return candidates.some((k) => k.spki_sha256.toLowerCase() === observed)
      ? line(ID_TITLES, 'id-6', 'pass', `observed TLS SPKI ${observed} is in the attested keyset`)
      : line(ID_TITLES, 'id-6', 'fail', `observed TLS SPKI ${observed} is not in the attested keyset (§1.1)`);
  }
  if (options.online) {
    return line(
      ID_TITLES,
      'id-6',
      'fail',
      'the channel used is not bound to the attested keyset: browsers cannot observe the TLS certificate — supply the SPKI your own stack observed, or use the `aci` CLI / `aci serve` proxy (§1.1, §9.1(6))',
    );
  }
  return line(
    ID_TITLES,
    'id-6',
    'skip',
    'no live channel to bind (offline audit of a stored report)',
    'no live channel to bind',
  );
}

export interface ReceiptTranscript {
  lines: TranscriptLine[];
  verdict: Verdict;
}

/** Aggregator inputs for the §9.3(5)-(6) checks. */
export interface UpstreamAuditInput {
  /** The session record the receipt cites, as served (§9.2). */
  session?: unknown;
  /** Session ids the client pinned with `provider.aci_session_ids` (§5.3). */
  pinnedSessions?: string[];
  /**
   * Whether the client requires verified serving. §9.3(5) rejects
   * `required: false` only for such a client; default true.
   */
  requiresVerified?: boolean;
  /**
   * The report's `service_capabilities.serving` (§4.1); default
   * `"aggregator"` (Appendix B). Only a `direct` service may omit
   * `upstream.verified` (§7.5).
   */
  serving?: string;
}

/**
 * Run the §9.3 checks against a receipt document and the keyset the report
 * verification established. Byte inputs are optional: absent bytes make receipt-3/receipt-4
 * skips, not passes. `upstream` supplies the aggregator inputs for §9.3(5)-(6);
 * without it those checks are skips with their reason, never silent passes.
 */
export async function receiptTranscript(
  envelope: ReceiptEnvelope,
  keyset: WorkloadKeyset,
  establishedDigest: string,
  requestBytes?: Uint8Array | string,
  responseBytes?: Uint8Array | string,
  upstream?: UpstreamAuditInput,
): Promise<ReceiptTranscript> {
  const result = await verifyReceipt(envelope, keyset, establishedDigest);
  const lines: TranscriptLine[] = [];

  const sig = libCheck(result.checks, 'signature');
  lines.push(
    sig?.ok
      ? line(RECEIPT_TITLES, 'receipt-1', 'pass', `key "${envelope.key_id}"`)
      : line(RECEIPT_TITLES, 'receipt-1', 'fail', sig?.detail ?? 'signature verification failed'),
  );

  // receipt-2 covers both §9.3(2) clauses: the Appendix B api_version gate and the
  // keyset-digest binding. A foreign version must reach the verdict.
  const version = libCheck(result.checks, 'api_version');
  const digest = libCheck(result.checks, 'workload_keyset_digest');
  lines.push(
    version?.ok === false
      ? line(RECEIPT_TITLES, 'receipt-2', 'fail', version.detail ?? 'api_version is not "aci/1"')
      : digest?.ok
        ? line(RECEIPT_TITLES, 'receipt-2', 'pass', 'the document binds to the verified keyset')
        : line(RECEIPT_TITLES, 'receipt-2', 'fail', digest?.detail ?? 'binding mismatch'),
  );

  if (result.payload === undefined || requestBytes === undefined) {
    lines.push(line(RECEIPT_TITLES, 'receipt-3', 'skip', 'request bytes not supplied', 'request bytes not supplied'));
  } else {
    const ok = await checkRequestBodyHash(result.payload, requestBytes);
    lines.push(
      line(RECEIPT_TITLES, 'receipt-3', ok ? 'pass' : 'fail', ok ? undefined : 'request.received.body_hash does not match the supplied bytes'),
    );
  }

  // receipt-note — the §9.3 rewrite observation the CLI also prints: a
  // request.forwarded hash differing from request.received is the service-side
  // rewrite; whether one is acceptable is the caller's policy.
  if (result.payload !== undefined) {
    const received = findEvent(result.payload, 'request.received')?.body_hash;
    const forwarded = findEvent(result.payload, 'request.forwarded')?.body_hash;
    if (
      typeof received === 'string' &&
      typeof forwarded === 'string' &&
      received !== forwarded
    ) {
      lines.push({
        id: 'receipt-note',
        section: '9.3',
        title: 'service-side rewrite observed',
        status: 'info',
        detail: `request.forwarded differs from request.received (${forwarded})`,
      });
    }
  }

  if (result.payload === undefined || responseBytes === undefined) {
    lines.push(line(RECEIPT_TITLES, 'receipt-4', 'skip', 'response bytes not supplied', 'response bytes not supplied'));
  } else {
    const ok = await checkResponseBodyHash(result.payload, responseBytes);
    lines.push(
      line(RECEIPT_TITLES, 'receipt-4', ok ? 'pass' : 'fail', ok ? undefined : 'response.returned.body_hash does not match the supplied bytes'),
    );
  }

  lines.push(...(await upstreamLines(result.payload, upstream)));

  return { lines, verdict: computeVerdict(lines) };
}

/**
 * §9.3(5)-(6): the serving upstream was verified and cites a session, and that
 * session verifies (§9.2(1)-(2)), covers `served_at`, and is one the client
 * pinned. Each check that cannot run is a skip with its reason.
 */
async function upstreamLines(
  payload: ReceiptPayload | undefined,
  upstream: UpstreamAuditInput | undefined,
): Promise<TranscriptLine[]> {
  const u = (id: 'upstream-1' | 'upstream-2', status: CheckStatus, detail: string, reason?: string) =>
    line(UPSTREAM_TITLES, id, status, detail, reason);
  const inputSupplied = upstream !== undefined;
  upstream = upstream ?? {};
  if (payload === undefined) {
    const why = 'the receipt did not verify';
    return [u('upstream-1', 'skip', why, why), u('upstream-2', 'skip', why, why)];
  }

  const strict = upstream.requiresVerified ?? true;
  const events = (payload.event_log ?? []).filter((e) => e.type === 'upstream.verified');
  const verified = events.find((e) => e.result === 'verified');
  const cited = typeof verified?.session_id === 'string' ? verified.session_id : undefined;

  // §9.3(5). Only aggregator receipts carry this event (§7.5), so its absence
  // means the check does not apply — a skip, never a silent pass.
  const serving = upstream.serving;
  const u1 =
    events.length === 0
      ? serving === 'direct'
        ? u('upstream-1', 'pass', 'direct service (§4.1): no upstream hop; the §9.1-verified workload serves')
        : serving === undefined
          ? u(
              'upstream-1',
              'skip',
              'no upstream.verified event; pass `serving` from the verified report to decide §7.5 conformance',
              'serving mode not supplied',
            )
          : u(
              'upstream-1',
              'fail',
              `receipt carries no upstream.verified event, and serving=${JSON.stringify(serving)} is not "direct" (§7.5)`,
            )
      : verified === undefined
        ? u('upstream-1', strict ? 'fail' : 'info', 'the receipt records unverified serving')
        : strict && verified.required !== true
          ? u('upstream-1', 'fail', 'verified upstream but required is not true')
          : cited === undefined
            ? u('upstream-1', 'fail', 'verified upstream but cites no session_id')
            : u('upstream-1', 'pass', `session ${cited}`);

  // §9.3(6) membership needs only the receipt, so it is checked before the
  // record; §9.2(1)-(2) need the record itself.
  if (cited === undefined) return [u1, u('upstream-2', 'skip', 'no cited session', 'no cited session')];
  if (upstream.pinnedSessions && upstream.pinnedSessions.length === 0) {
    return [u1, u('upstream-2', 'fail', 'pinnedSessions must be non-empty when supplied (§5.3)')];
  }
  if (upstream.pinnedSessions && !upstream.pinnedSessions.includes(cited)) {
    return [u1, u('upstream-2', 'fail', `cited session ${cited} is not in the pinned list (§5.3)`)];
  }
  if (upstream.session === undefined) {
    // §8 retention: a client that demanded verified serving cannot accept an
    // unauditable cited session. Applies only when the caller supplied the
    // aggregator inputs — i.e. actually tried to fetch the record.
    if (strict && inputSupplied) {
      return [u1, u('upstream-2', 'fail', 'the cited session could not be audited: no record supplied (§8 retention)')];
    }
    const why = 'no session record supplied';
    return [u1, u('upstream-2', 'skip', why, why)];
  }

  const record = upstream.session as SessionRecord;
  const problems: string[] = [];
  if ((await computeSessionId(record)) !== cited) problems.push('does not hash to the cited id');
  if (!checkSessionApiVersion(record)) problems.push('api_version is not "aci/1"');
  if (!(payload.served_at >= record.established_at && payload.served_at <= record.expires_at)) {
    problems.push('served_at is outside the validity window');
  }
  if (!(await checkSessionEvidence(record.evidence))) problems.push('evidence does not hash');
  return [
    u1,
    problems.length === 0
      ? u('upstream-2', 'pass', `session ${cited} verifies`)
      : u('upstream-2', 'fail', `session ${cited}: ${problems.join('; ')}`),
  ];
}
