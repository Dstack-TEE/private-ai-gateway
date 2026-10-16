import { visit } from 'jsonc-parser';
import { AciFormatError } from './errors.js';

// Keep these limits in sync with aci-verify/src/dstack/event_log.rs.
export const MAX_EVENT_LOG_BYTES = 1024 * 1024;
export const MAX_EVENT_LOG_DEPTH = 16;
export const DSTACK_RUNTIME_EVENT_TYPE = 0x08000001;

export interface DstackEvent {
  imr: number;
  event_type: number;
  digest: string;
  event: string;
  event_payload: string;
}

/** Reject lone surrogates before UTF-8 encoding could replace them. */
export function checkDstackString(value: string, label: string): void {
  if (/\p{Cs}/u.test(value)) throw new AciFormatError(`${label} contains ill-formed Unicode`);
}

export function parseDstackEventLog(eventLog: string): DstackEvent[] {
  checkDstackString(eventLog, 'dstack event_log');
  if (
    eventLog.length > MAX_EVENT_LOG_BYTES ||
    new TextEncoder().encode(eventLog).length > MAX_EVENT_LOG_BYTES
  ) {
    throw new AciFormatError('dstack event_log exceeds maximum byte size');
  }
  const frames: (Set<string> | null)[] = [];
  function begin(keys: Set<string> | null): void {
    if (frames.length >= MAX_EVENT_LOG_DEPTH) {
      throw new AciFormatError('dstack event_log exceeds maximum nesting depth');
    }
    frames.push(keys);
  }
  // Visit the original tokens before JSON.parse can discard duplicate keys or
  // normalize float/exponent spellings. Throwing in a visitor stops descent.
  visit(
    eventLog,
    {
      onObjectBegin: () => begin(new Set()),
      onArrayBegin: () => begin(null),
      onObjectEnd: () => {
        frames.pop();
      },
      onArrayEnd: () => {
        frames.pop();
      },
      onObjectProperty: (key) => {
        checkDstackString(key, 'dstack event field');
        const keys = frames[frames.length - 1];
        if (!keys || keys.has(key))
          throw new AciFormatError('duplicate dstack event_log object key');
        keys.add(key);
      },
      onLiteralValue: (value: unknown, offset, length, _line, _column, pathSupplier) => {
        const path = pathSupplier();
        if (typeof value === 'string') checkDstackString(value, 'dstack event field');
        if (typeof value !== 'number') return;
        if (!Number.isFinite(value)) throw new AciFormatError('invalid dstack event_log number');
        if (path.length === 2 && ['imr', 'event_type', 'version'].includes(String(path[1]))) {
          const token = eventLog.slice(offset, offset + length);
          if (!/^(0|[1-9][0-9]*)$/.test(token) || value > 0xffffffff) {
            throw new AciFormatError(
              'dstack event_log integer must be an unsigned u32 JSON integer token',
            );
          }
        }
      },
      onError: () => {
        throw new AciFormatError('dstack event_log is not valid JSON');
      },
    },
    { disallowComments: true, allowTrailingComma: false, allowEmptyContent: false },
  );
  const parsed: unknown = JSON.parse(eventLog);
  if (!Array.isArray(parsed)) throw new AciFormatError('dstack event_log must be an array');
  return parsed.map((value: unknown) => {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
      throw new AciFormatError('invalid dstack event_log entry');
    }
    const {
      version,
      imr,
      event_type: eventType,
      digest,
      event,
      event_payload: payload,
    } = value as Record<string, unknown>;
    if (Object.hasOwn(value, 'version') && version !== 1) {
      throw new AciFormatError(
        'unsupported dstack event log version: expected version absent or numeric 1 (v1)',
      );
    }
    if (
      typeof imr !== 'number' ||
      typeof eventType !== 'number' ||
      typeof digest !== 'string' ||
      typeof event !== 'string' ||
      typeof payload !== 'string'
    )
      throw new AciFormatError('invalid dstack event_log entry');
    if (imr === 3 && eventType !== DSTACK_RUNTIME_EVENT_TYPE) {
      throw new AciFormatError('dstack RTMR3 contains a non-runtime event');
    }
    // All names emitted by dstack v0.5.9/v0.6.1 use lowercase kebab case.
    // Restrict custom names too: ':' makes the v1 preimage ambiguous.
    if (eventType === DSTACK_RUNTIME_EVENT_TYPE && !/^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(event)) {
      throw new AciFormatError('invalid dstack runtime event name');
    }
    return { imr, event_type: eventType, digest, event, event_payload: payload };
  });
}
