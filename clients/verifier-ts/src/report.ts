/**
 * Report binding checks a verifier can run with pure Web Crypto — §10.1 check 2
 * (binding and freshness: keyset bytes → digest → statement → `report_data`)
 * and check 3 (expiry), plus the aci/1 protocol gate. Check 1 (the hardware
 * quote verifies to the vendor root and binds `report_data`) is done by
 * {@link verifyQuote} via @phala/dcap-qvl; checks 5–6 (custody, channel) stay
 * profile / caller territory.
 */

import { getCollateralAndVerify } from '@phala/dcap-qvl';
import { computeKeysetDigest, computeReportData } from './digest.js';
import { fromBase64, fromHex, sha256Hex, sha384 } from './crypto.js';
import { AciFormatError } from './errors.js';
import type { AttestationReport, Check, ReportVerification, WorkloadKeyset } from './types.js';

/** rt_mr3 lives at this byte offset of a v4 TDX quote: 48-byte header + the
 *  TDReport10 fields up to rt_mr3 (472 bytes). */
const TDX_RTMR3_OFFSET = 520;

interface DstackEvent {
  imr: number;
  digest: string;
  event: string;
  event_payload: string;
}

/** Options for {@link verifyReportBinding}. */
export interface ReportBindingOptions {
  /**
   * Current time in Unix seconds for the expiry check (§10.1 check 3).
   * Defaults to the local clock; pass an explicit value for deterministic tests.
   */
  now?: number;
}

/**
 * Verify the report's cryptographic bindings for `nonce` — the value this
 * client sent to `GET /v1/aci/attestation`, or `null`/`undefined` when it sent
 * none (§4.2). One recomputation establishes that the keyset is exactly what
 * the quote bound and that the quote postdates the challenge (§10.1 check 2).
 *
 * Returns per-check results plus the established keyset (digest, exact bytes,
 * parsed form); a failed check is `ok: false`, never thrown.
 */
export async function verifyReportBinding(
  report: AttestationReport,
  nonce: string | null | undefined,
  options: ReportBindingOptions = {},
): Promise<ReportVerification> {
  const now = options.now ?? Math.floor(Date.now() / 1000);
  const checks: Check[] = [];

  // Protocol gate (Appendix A): artifacts with another version are rejected.
  const versionOk = report.api_version === 'aci/1';
  checks.push({
    name: 'api_version',
    ok: versionOk,
    ...(versionOk ? {} : { detail: `api_version "${report.api_version}" is not "aci/1"` }),
  });

  let keysetBytes: Uint8Array | undefined;
  try {
    keysetBytes = fromBase64(report.attestation.workload_keyset_b64);
  } catch {
    // Handled below: without the keyset bytes no binding check can run.
  }
  if (keysetBytes === undefined) {
    const detail = 'workload_keyset_b64 does not decode as base64';
    for (const name of ['workload_keyset_digest', 'report_data', 'not_after']) {
      checks.push({ name, ok: false, detail });
    }
    return { ok: false, checks };
  }

  // §10.1 check 2: recompute the whole chain from the served bytes. The
  // recomputed digest is authoritative (§3) — the report's restated copy is
  // checked for consistency but never feeds the statement.
  const digest = await computeKeysetDigest(keysetBytes);
  pushEqual(checks, 'workload_keyset_digest', report.workload_keyset_digest, digest);
  const expectedReportData = await computeReportData(digest, nonce);
  pushEqual(checks, 'report_data', report.attestation.report_data, expectedReportData);

  let keyset: WorkloadKeyset | undefined;
  try {
    keyset = JSON.parse(new TextDecoder().decode(keysetBytes)) as WorkloadKeyset;
  } catch {
    // Handled below: the bytes are the artifact, but expiry needs the JSON.
  }

  // §10.1 check 3: now < not_after in the decoded keyset.
  if (keyset === undefined || typeof keyset.not_after !== 'number') {
    checks.push({
      name: 'not_after',
      ok: false,
      detail:
        keyset === undefined
          ? 'keyset bytes are not valid JSON'
          : 'keyset has no numeric not_after',
    });
  } else {
    const ok = now < keyset.not_after;
    checks.push({
      name: 'not_after',
      ok,
      ...(ok ? {} : { detail: `now ${now} >= not_after ${keyset.not_after}` }),
    });
  }

  return {
    ok: checks.every((c) => c.ok),
    checks,
    workloadKeysetDigest: digest,
    keysetBytes,
    ...(keyset !== undefined ? { keyset } : {}),
  };
}

function pushEqual(checks: Check[], name: string, actual: string, expected: string): void {
  const ok = actual === expected;
  checks.push({ name, ok, ...(ok ? {} : { detail: `report ${actual} != recomputed ${expected}` }) });
}

/**
 * §10.1 check 4 (dstack profile): the booted docker-compose is the one measured
 * into the report's stated RTMR3. Replays `evidence.event_log` to RTMR3 (SHA-384
 * chain over each `imr==3` digest from a 48-byte-zero start), checks it equals
 * the RTMR3 the raw TDX quote states, then checks `sha256(app_compose)` equals
 * the measured `compose-hash`. Proves the compose against the quote's *stated*
 * RTMR3 only — a genuine, TCB-current quote needs a quote verifier (dcap-qvl.js /
 * the `aci` CLI), and whether the compose is acceptable is caller policy. Throws
 * {@link AciFormatError} only for malformed evidence, never for a failed check.
 */
export async function verifyComposeMeasurement(
  report: AttestationReport,
): Promise<{ ok: boolean; checks: Check[] }> {
  const ev = (report.attestation.evidence ?? {}) as Record<string, unknown>;
  const { event_log: eventLog, app_compose: appCompose, quote } = ev;
  if (typeof eventLog !== 'string' || typeof appCompose !== 'string' || typeof quote !== 'string') {
    throw new AciFormatError('evidence needs string event_log, app_compose, and quote');
  }
  const events = JSON.parse(eventLog) as DstackEvent[];

  // The event log must replay to the RTMR3 the raw quote states (v4 TDX offset).
  const replayed = await replayRtmr3(events);
  const stated = fromHex(quote).slice(TDX_RTMR3_OFFSET, TDX_RTMR3_OFFSET + 48);
  const rtmrOk = stated.length === 48 && replayed.every((b, i) => b === stated[i]);

  // sha256(app_compose) must equal the compose-hash measured before system-ready.
  const gate = events.find((e) => e.imr === 3 && (e.event === 'compose-hash' || e.event === 'system-ready'));
  const measured = gate?.event === 'compose-hash' ? gate.event_payload : undefined;
  const recomputed = (await sha256Hex(new TextEncoder().encode(appCompose))).toLowerCase();
  const composeOk = measured?.toLowerCase() === recomputed;

  return {
    ok: rtmrOk && composeOk,
    checks: [
      { name: 'rtmr3', ok: rtmrOk, ...(rtmrOk ? {} : { detail: 'event log RTMR3 != quote RTMR3' }) },
      { name: 'compose_hash', ok: composeOk, ...(composeOk ? {} : { detail: `sha256(app_compose)=${recomputed} != measured ${measured ?? '(none)'}` }) },
    ],
  };
}

/** Replay the dstack event log's `imr==3` events to RTMR3 (SHA-384 chain over
 *  each digest, zero-padded to 48 bytes). */
async function replayRtmr3(events: DstackEvent[]): Promise<Uint8Array> {
  let mr: Uint8Array = new Uint8Array(48);
  for (const e of events) {
    if (e.imr !== 3) continue;
    const digest = fromHex(e.digest);
    const buf = new Uint8Array(48 + Math.max(digest.length, 48));
    buf.set(mr);
    buf.set(digest, 48);
    mr = await sha384(buf);
  }
  return mr;
}

/**
 * §10.1 check 1 (L2.1): verify the TDX quote to the Intel vendor root with
 * @phala/dcap-qvl — it fetches collateral from the default Phala PCCS (override
 * with `pccsUrl`) — then confirm the verified quote's report_data equals the
 * report's `report_data` zero-padded to 64 bytes (§4.2) and that the TCB is up
 * to date. A pass here makes the RTMR3 that {@link verifyComposeMeasurement}
 * replays against authentic, so the two together prove genuine TEE + which code.
 * Returns a result, never throws for a failed quote (only the fetch/parse can).
 */
export async function verifyQuote(
  report: AttestationReport,
  pccsUrl?: string,
): Promise<{ ok: boolean; status?: string; detail?: string }> {
  const quote = (report.attestation.evidence as Record<string, unknown> | undefined)?.quote;
  if (typeof quote !== 'string') {
    return { ok: false, detail: 'report evidence carries no quote' };
  }
  let verified;
  try {
    verified = await getCollateralAndVerify(fromHex(quote), pccsUrl);
  } catch (e) {
    return { ok: false, detail: `quote did not verify: ${e instanceof Error ? e.message : String(e)}` };
  }
  const slot = new Uint8Array(64);
  slot.set(fromHex(report.attestation.report_data).slice(0, 32));
  const rd = (verified.report as { reportData?: Uint8Array }).reportData;
  if (!rd || rd.length !== 64 || !slot.every((b, i) => b === rd[i])) {
    return { ok: false, status: verified.status, detail: 'quote report_data does not bind the report' };
  }
  if (verified.status !== 'UpToDate') {
    return { ok: false, status: verified.status, detail: `TCB status ${verified.status}` };
  }
  return { ok: true, status: verified.status };
}
