import { visit } from 'jsonc-parser';
import { AciFormatError } from './errors.js';

// Keep these limits in sync with aci-verify/src/dstack/event_log.rs.
export const MAX_EVENT_LOG_BYTES = 1024 * 1024;
export const MAX_APP_COMPOSE_BYTES = 4 * 1024 * 1024;
export const MAX_EVENT_FIELD_BYTES = 128 * 1024;
export const MAX_EVENT_LOG_DEPTH = 16;
export const MAX_EVENT_LOG_ENTRIES = 1024;
export const DSTACK_RUNTIME_EVENT_TYPE = 0x08000001;

export interface DstackEvent {
  imr: number;
  event_type: number;
  digest: string;
  event: string;
  event_payload: string;
}

/** Count UTF-8 bytes without encoding or allocating a replacement for lone surrogates. */
export function checkDstackString(value: string, label: string, maxBytes: number): void {
  let bytes = 0;
  for (let i = 0; i < value.length; i++) {
    const code = value.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff) {
      const next = value.charCodeAt(++i);
      if (!(next >= 0xdc00 && next <= 0xdfff)) {
        throw new AciFormatError(`${label} contains ill-formed Unicode`);
      }
      bytes += 4;
    } else if (code >= 0xdc00 && code <= 0xdfff) {
      throw new AciFormatError(`${label} contains ill-formed Unicode`);
    } else {
      bytes += code < 0x80 ? 1 : code < 0x800 ? 2 : 3;
    }
    if (bytes > maxBytes) throw new AciFormatError(`${label} exceeds ${maxBytes} bytes`);
  }
}

export function parseDstackEventLog(eventLog: string): DstackEvent[] {
  checkDstackString(eventLog, 'dstack event_log', MAX_EVENT_LOG_BYTES);
  const frames: (Set<string> | null)[] = [];
  let entries = 0;
  function valueAt(path: (string | number)[]): void {
    if (path.length === 1 && typeof path[0] === 'number' && ++entries > MAX_EVENT_LOG_ENTRIES) {
      throw new AciFormatError('dstack event_log has too many entries');
    }
  }
  function begin(keys: Set<string> | null, path: (string | number)[]): void {
    valueAt(path);
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
      onObjectBegin: (_offset, _length, _line, _column, path) => begin(new Set(), path()),
      onArrayBegin: (_offset, _length, _line, _column, path) => begin(null, path()),
      onObjectEnd: () => {
        frames.pop();
      },
      onArrayEnd: () => {
        frames.pop();
      },
      onObjectProperty: (key) => {
        checkDstackString(key, 'dstack event field', MAX_EVENT_FIELD_BYTES);
        const keys = frames[frames.length - 1];
        if (!keys || keys.has(key))
          throw new AciFormatError('duplicate dstack event_log object key');
        keys.add(key);
      },
      onLiteralValue: (value: unknown, offset, length, _line, _column, pathSupplier) => {
        const path = pathSupplier();
        valueAt(path);
        if (typeof value === 'string')
          checkDstackString(value, 'dstack event field', MAX_EVENT_FIELD_BYTES);
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
      !Number.isInteger(imr) ||
      imr < 0 ||
      imr > 0xffffffff ||
      typeof eventType !== 'number' ||
      !Number.isInteger(eventType) ||
      eventType < 0 ||
      eventType > 0xffffffff ||
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
