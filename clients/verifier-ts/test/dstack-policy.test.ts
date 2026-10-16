import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { verifyComposeMeasurement, AciFormatError, type AttestationReport } from '../src/index.js';
import { MAX_APP_COMPOSE_BYTES } from '../src/dstack-event-log.js';
import { makeMeasuredComposeReport } from './fixtures.js';

interface Event {
  imr: number;
  event_type: number;
  digest: string;
  event: string;
  event_payload: string;
}
interface PolicyCase {
  name: string;
  valid: boolean;
  replacements?: [string, string][];
  app_compose_json?: string;
  unknown_field_bytes?: number;
  nested_arrays?: number;
  extra_boot_events?: number;
  pad_log_to?: number;
  synthetic_tail?: Event;
}
const recorded = JSON.parse(
  readFileSync(new URL('../../test/fixtures/dstack_v1_attestation.json', import.meta.url), 'utf8'),
) as { report: AttestationReport };
const cases = JSON.parse(
  readFileSync(new URL('../../test/fixtures/dstack_policy_cases.json', import.meta.url), 'utf8'),
) as PolicyCase[];

// Rust's dstack_policy integration test applies this same corpus to the same
// recorded evidence. Only explicitly synthetic tail cases alter RTMR3.
for (const vector of cases) {
  test(`dstack policy parity: ${vector.name}`, async () => {
    const report = structuredClone(recorded.report);
    const ev = report.attestation.evidence as {
      event_log: string;
      app_compose: string;
      quote: string;
    };
    for (const [from, to] of vector.replacements ?? []) {
      assert.ok(ev.event_log.includes(from));
      ev.event_log = ev.event_log.replace(from, to);
    }
    if (vector.app_compose_json !== undefined)
      ev.app_compose = JSON.parse(vector.app_compose_json) as string;
    if (vector.unknown_field_bytes !== undefined) {
      ev.event_log = ev.event_log.replace(
        '{',
        '{"extra":' + JSON.stringify('a'.repeat(vector.unknown_field_bytes)) + ',',
      );
    }
    if (vector.nested_arrays !== undefined) {
      ev.event_log = ev.event_log.replace(
        '{',
        '{"extra":' +
          '['.repeat(vector.nested_arrays) +
          '0' +
          ']'.repeat(vector.nested_arrays) +
          ',',
      );
    }
    if (vector.extra_boot_events !== undefined) {
      const log = JSON.parse(ev.event_log) as Event[];
      ev.event_log = JSON.stringify([
        ...Array.from({ length: vector.extra_boot_events }, () => log[0]),
        ...log,
      ]);
    }
    if (vector.pad_log_to !== undefined)
      ev.event_log += ' '.repeat(vector.pad_log_to - Buffer.byteLength(ev.event_log));
    if (vector.synthetic_tail) {
      const log = JSON.parse(ev.event_log) as Event[];
      log.push(vector.synthetic_tail);
      ev.event_log = JSON.stringify(log);
      let mr = Buffer.alloc(48);
      for (const event of log.filter((e) => e.imr === 3)) {
        mr = createHash('sha384').update(mr).update(Buffer.from(event.digest, 'hex')).digest();
      }
      const quote = Buffer.from(ev.quote, 'hex');
      quote.set(mr, 520);
      ev.quote = quote.toString('hex');
    }
    let ok: boolean;
    try {
      const result = await verifyComposeMeasurement(report);
      ok = result.ok;
      if (!ok) assert.equal(result.composeHash, undefined);
    } catch (error) {
      assert.ok(error instanceof AciFormatError, String(error));
      if (vector.valid) throw error;
      ok = false;
    }
    assert.equal(ok, vector.valid);
  });
}

test('compose byte limits count UTF-8 bytes and reject lone surrogates before hashing', async () => {
  const app = 'é'.repeat(MAX_APP_COMPOSE_BYTES / 2);
  const { report } = await makeMeasuredComposeReport(app);
  assert.equal((await verifyComposeMeasurement(report)).ok, true);
  const ev = report.attestation.evidence as { app_compose: string };
  ev.app_compose += 'a';
  await assert.rejects(verifyComposeMeasurement(report), /exceeds .* bytes/);
  const replacement = await makeMeasuredComposeReport('\ufffd');
  (replacement.report.attestation.evidence as { app_compose: string }).app_compose = '\ud800';
  await assert.rejects(verifyComposeMeasurement(replacement.report), /ill-formed Unicode/);
});
