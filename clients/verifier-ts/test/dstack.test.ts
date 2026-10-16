import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { once } from 'node:events';
import { readFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { test } from 'node:test';
import {
  reportTranscript,
  verifyComposeMeasurement,
  type AttestationReport,
} from '../src/index.js';
import { makeMeasuredComposeReport } from './fixtures.js';

interface Event {
  imr: number;
  event_type: number;
  digest: string;
  event: string;
  event_payload: string;
  version?: unknown;
}

interface Evidence {
  quote: string;
  event_log: string;
  app_compose: string;
}

const recorded = JSON.parse(
  readFileSync(new URL('../../test/fixtures/dstack_v1_attestation.json', import.meta.url), 'utf8'),
) as {
  nonce: string;
  captured_at: string;
  report: AttestationReport;
  pccs_responses: {
    url: string;
    status: number;
    headers: Record<string, string>;
    body_base64: string;
  }[];
};

function evidence(report: AttestationReport): Evidence {
  return report.attestation.evidence as Evidence;
}

function events(report: AttestationReport): Event[] {
  return JSON.parse(evidence(report).event_log) as Event[];
}

function composeEvent(log: Event[]): Event {
  const event = log.find((e) => e.imr === 3 && e.event === 'compose-hash');
  assert.ok(event);
  return event;
}

// Only synthetic quotes are rebound here: matching RTMR3 lets the tests
// isolate event selection rules independently of quote authentication.
function setSyntheticEventLog(report: AttestationReport, log: Event[]): void {
  const ev = evidence(report);
  let rtmr3 = Buffer.alloc(48);
  for (const event of log.filter((e) => e.imr === 3)) {
    const digest = Buffer.from(event.digest.replace(/^0x/, ''), 'hex');
    rtmr3 = createHash('sha384')
      .update(rtmr3)
      .update(digest)
      .update(Buffer.alloc(Math.max(0, 48 - digest.length)))
      .digest();
  }
  const quote = Buffer.from(ev.quote, 'hex');
  quote.set(rtmr3, 520);
  ev.quote = quote.toString('hex');
  ev.event_log = JSON.stringify(log);
}

test('recorded vendor-valid v1 quote binds the displayed compose to RTMR3', async (t) => {
  // Serve recorded Intel collateral locally and freeze its validation time.
  // All quote signatures, vendor roots and report-data bindings are verified
  // by the real dcap-qvl implementation, without external network access.
  const now = Math.floor(Date.parse(recorded.captured_at) / 1000);
  t.mock.method(Date, 'now', () => now * 1000);
  const responses = new Map(
    recorded.pccs_responses.map((r) => {
      const url = new URL(r.url);
      return [url.pathname + url.search, r];
    }),
  );
  const server = createServer((request, response) => {
    const saved = responses.get(request.url ?? '');
    if (!saved) {
      response.writeHead(404).end();
      return;
    }
    response.writeHead(saved.status, saved.headers);
    response.end(Buffer.from(saved.body_base64, 'base64'));
  });
  const listening = once(server, 'listening');
  server.listen(0, '127.0.0.1');
  await listening;
  t.after(
    () =>
      new Promise<void>((resolve, reject) => {
        server.close((error) => (error ? reject(error) : resolve()));
        server.closeAllConnections();
      }),
  );
  const address = server.address();
  assert.ok(address && typeof address === 'object');
  const options = { now, pccsUrl: `http://127.0.0.1:${address.port}` };

  const unchanged = await reportTranscript(recorded.report, recorded.nonce, options);
  for (const id of ['id-1', 'id-2', 'id-3', 'id-4']) {
    const line = unchanged.lines.find((l) => l.id === id);
    assert.equal(line?.status, 'pass', JSON.stringify(line));
  }
  assert.equal(unchanged.composeHash, composeEvent(events(recorded.report)).event_payload);

  const cases: { name: string; mutate: (ev: Evidence, log: Event[]) => void; reason: RegExp }[] = [
    {
      name: 'changed app-compose',
      mutate: (ev) => {
        ev.app_compose += '\n';
      },
      reason: /sha256\(app_compose\)/,
    },
    {
      name: 'changed compose payload',
      mutate: (_ev, log) => {
        composeEvent(log).event_payload = 'ab'.repeat(32);
      },
      reason: /digest does not match/,
    },
    {
      name: 'changed app-compose and its displayed hash',
      mutate: (ev, log) => {
        ev.app_compose += '\n';
        composeEvent(log).event_payload = createHash('sha256').update(ev.app_compose).digest('hex');
      },
      reason: /digest does not match/,
    },
    {
      name: 'changed stored runtime digest',
      mutate: (_ev, log) => {
        composeEvent(log).digest = '00'.repeat(48);
      },
      reason: /digest does not match/,
    },
    {
      name: 'changed runtime event name',
      mutate: (_ev, log) => {
        composeEvent(log).event = 'other';
      },
      reason: /digest does not match/,
    },
    {
      name: 'consistent changed payload and digest still disagree with the quote',
      mutate: (ev, log) => {
        ev.app_compose += '\n';
        const compose = composeEvent(log);
        compose.event_payload = createHash('sha256').update(ev.app_compose).digest('hex');
        compose.digest = createHash('sha384')
          .update(Buffer.from('010000083a', 'hex'))
          .update(compose.event)
          .update(':')
          .update(Buffer.from(compose.event_payload, 'hex'))
          .digest('hex');
      },
      reason: /RTMR3 != quote RTMR3/,
    },
    {
      name: 'unknown version on an event outside RTMR3',
      mutate: (_ev, log) => {
        const boot = log.find((e) => e.imr !== 3);
        assert.ok(boot);
        boot.version = 99;
      },
      reason: /only v1 \(version absent\) is supported/,
    },
  ];
  for (const version of [1, 2, 99, '2', null]) {
    cases.push({
      name: `explicit event version ${JSON.stringify(version)}`,
      mutate: (_ev, log) => {
        composeEvent(log).version = version;
      },
      reason: /only v1 \(version absent\) is supported/,
    });
  }
  for (const { name, mutate, reason } of cases) {
    await t.test(name, async () => {
      const report = structuredClone(recorded.report);
      const ev = evidence(report);
      const log = events(report);
      mutate(ev, log);
      ev.event_log = JSON.stringify(log);
      const result = await reportTranscript(report, recorded.nonce, options);
      assert.equal(result.lines.find((l) => l.id === 'id-1')?.status, 'pass');
      const id4 = result.lines.find((l) => l.id === 'id-4');
      assert.equal(id4?.status, 'fail', JSON.stringify(id4));
      assert.match(id4.detail ?? '', reason);
      assert.equal(result.composeHash, undefined);
    });
  }
});

test('duplicate pre-system-ready compose events fail even when RTMR3 matches', async () => {
  const { report } = await makeMeasuredComposeReport();
  const log = events(report);
  log.unshift({ ...composeEvent(log) });
  setSyntheticEventLog(report, log);
  const result = await verifyComposeMeasurement(report);
  assert.equal(result.ok, false);
  assert.equal(result.checks.find((c) => c.name === 'rtmr3')?.ok, true);
  assert.match(
    result.checks.find((c) => c.name === 'compose_hash')?.detail ?? '',
    /multiple pre-system-ready/,
  );
  assert.equal(result.composeHash, undefined);
});

test('only RTMR3 runtime events can supply compose-hash or system-ready', async () => {
  const { report, composeHash } = await makeMeasuredComposeReport();
  const log = events(report);
  const compose = composeEvent(log);
  const decoys = [
    {
      imr: 3,
      event_type: 1,
      digest: '11'.repeat(32),
      event: 'system-ready',
      event_payload: 'not hex',
    },
    {
      imr: 3,
      event_type: 1,
      digest: '22'.repeat(64),
      event: 'compose-hash',
      event_payload: 'not hex',
    },
    { ...compose, imr: 2, digest: 'not hex', event_payload: 'not hex' },
  ];
  log.unshift(...decoys);
  setSyntheticEventLog(report, log);
  const result = await verifyComposeMeasurement(report);
  assert.equal(result.ok, true, JSON.stringify(result.checks));
  assert.equal(result.composeHash, composeHash);

  // A non-runtime event with a compose-hash label is never a measurement.
  compose.event_type = 1;
  setSyntheticEventLog(report, log);
  const missing = await verifyComposeMeasurement(report);
  assert.equal(missing.ok, false);
  assert.equal(missing.checks.find((c) => c.name === 'rtmr3')?.ok, true);
  assert.equal(missing.checks.find((c) => c.name === 'compose_hash')?.ok, false);
});

test('non-runtime system-ready cannot hide duplicate compose events; post-ready events are not selected', async () => {
  const { report, composeHash } = await makeMeasuredComposeReport();
  const log = events(report);
  const compose = composeEvent(log);
  log.splice(
    1,
    0,
    { imr: 3, event_type: 1, digest: '11'.repeat(48), event: 'system-ready', event_payload: '' },
    { ...compose },
  );
  setSyntheticEventLog(report, log);
  assert.equal((await verifyComposeMeasurement(report)).ok, false);

  log.splice(2, 1);
  log.push({ ...compose });
  setSyntheticEventLog(report, log);
  assert.equal((await verifyComposeMeasurement(report)).composeHash, composeHash);

  log.splice(log.indexOf(compose), 1);
  setSyntheticEventLog(report, log);
  assert.equal((await verifyComposeMeasurement(report)).ok, false);
});

test('malformed runtime hex fails closed; Rust-compatible hex encodings still pass', async () => {
  const { report, composeHash } = await makeMeasuredComposeReport();
  const log = events(report);
  const compose = composeEvent(log);
  const digest = compose.digest;
  compose.event_payload = '0x' + composeHash.toUpperCase();
  compose.digest = '0x' + digest.toUpperCase();
  evidence(report).event_log = JSON.stringify(log);
  assert.equal((await verifyComposeMeasurement(report)).composeHash, composeHash);

  for (const field of ['digest', 'event_payload'] as const) {
    compose.digest = digest;
    compose.event_payload = composeHash;
    compose[field] = '1z' + compose[field].slice(2);
    evidence(report).event_log = JSON.stringify(log);
    await assert.rejects(verifyComposeMeasurement(report), /not valid hex/);
  }
});

test('malformed event-log entries fail closed before replay', async () => {
  const { report } = await makeMeasuredComposeReport();
  for (const eventLog of ['{', '{}', '[null]', '[{}]', '[{"imr":3,"event_type":-1}]']) {
    evidence(report).event_log = eventLog;
    await assert.rejects(verifyComposeMeasurement(report), /dstack event_log/);
  }
});
