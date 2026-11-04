// SPDX-FileCopyrightText: © 2025 Phala Network <dstack@phala.network>
// SPDX-License-Identifier: Apache-2.0
//
// Port of Dstack-TEE/dstack dstack/cc-eventlog at verifier-v0.6.3
// (Apache-2.0): TdxEvent, RuntimeEvent::preimage and validate_v2_preimages.
import { fromHex, jcsBytes, sha384, toHex } from './crypto.js';
import { AciFormatError } from './errors.js';

export const DSTACK_RUNTIME_EVENT_TYPE = 0x08000001;

export interface DstackEvent {
  imr: number;
  event_type: number;
  digest: Uint8Array;
  event: string;
  event_payload: Uint8Array;
  version: 1 | 2;
  preimage?: string;
}

function hexBytes(value: unknown): Uint8Array {
  // Upstream uses hex::decode. fromHex alone also accepts partial hex bytes.
  if (typeof value !== 'string' || !/^(?:[0-9a-fA-F]{2})*$/.test(value)) {
    throw new AciFormatError('invalid dstack event hex');
  }
  return fromHex(value);
}

function u32(value: unknown): value is number {
  return typeof value === 'number' && Number.isInteger(value) && value >= 0 && value <= 0xffffffff;
}

/** Rust strings cannot contain lone surrogates; TextEncoder would replace them. */
function validString(value: unknown): value is string {
  return typeof value === 'string' && !/\p{Cs}/u.test(value);
}

export function parseDstackEventLog(eventLog: string): DstackEvent[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(eventLog);
  } catch {
    throw new AciFormatError('dstack event_log is not valid JSON');
  }
  if (!Array.isArray(parsed)) throw new AciFormatError('dstack event_log must be an array');
  return parsed.map((value: unknown) => {
    if (value === null || typeof value !== 'object' || Array.isArray(value)) {
      throw new AciFormatError('invalid dstack event_log entry');
    }
    const {
      imr,
      event_type: eventType,
      event,
      event_payload: payload,
      preimage,
    } = value as Record<string, unknown>;
    const fields = value as Record<string, unknown>;
    const version = Object.hasOwn(fields, 'version') ? fields.version : 1;
    if (!u32(imr) || !u32(eventType) || !validString(event)) {
      throw new AciFormatError('invalid dstack event_log entry');
    }
    if (version !== 1 && version !== 2) {
      throw new AciFormatError('unsupported dstack event log version');
    }
    if (preimage != null && !validString(preimage)) {
      throw new AciFormatError('invalid dstack event preimage');
    }
    return {
      imr,
      event_type: eventType,
      event,
      event_payload: hexBytes(payload),
      digest: hexBytes(Object.hasOwn(fields, 'digest') ? fields.digest : ''),
      version,
      ...(preimage == null ? {} : { preimage }),
    };
  });
}

function runtimePreimage(event: DstackEvent): Uint8Array {
  if (event.version === 2) {
    // These are fixed ASCII keys, a fixed integer and strings: the existing
    // JCS helper provides exactly upstream's RFC 8785 representation.
    return jcsBytes({
      name: event.event,
      payload: toHex(event.event_payload),
      type: DSTACK_RUNTIME_EVENT_TYPE,
    });
  }
  const name = new TextEncoder().encode(event.event);
  const preimage = new Uint8Array(6 + name.length + event.event_payload.length);
  new DataView(preimage.buffer).setUint32(0, DSTACK_RUNTIME_EVENT_TYPE, true);
  preimage[4] = 0x3a;
  preimage.set(name, 5);
  preimage[5 + name.length] = 0x3a;
  preimage.set(event.event_payload, 6 + name.length);
  return preimage;
}

function equal(left: Uint8Array, right: Uint8Array): boolean {
  return left.length === right.length && left.every((byte, index) => byte === right[index]);
}

/** Validate V2 preimages, bind RTMR3 fields to their advertised digests, then
 * replay recomputed digests (cc_eventlog::replay_events::<Sha384>). */
export async function replayDstackRtmr3(events: DstackEvent[]): Promise<Uint8Array> {
  let mr: Uint8Array = new Uint8Array(48);
  for (const [index, event] of events.entries()) {
    const runtime = event.event_type === DSTACK_RUNTIME_EVENT_TYPE;
    const preimage = runtime ? runtimePreimage(event) : undefined;
    if (preimage && event.version === 2) {
      if (event.preimage === undefined) {
        throw new AciFormatError(`V2 runtime event ${index} is missing its digest preimage`);
      }
      const supplied = hexBytes(event.preimage);
      if (!equal(await sha384(supplied), event.digest)) {
        throw new AciFormatError(`V2 runtime event ${index} digest does not match its preimage`);
      }
      if (!equal(supplied, preimage)) {
        throw new AciFormatError(
          `V2 runtime event ${index} preimage is not the canonical event representation`,
        );
      }
    }
    if (event.imr !== 3) continue;
    if (!preimage) {
      throw new AciFormatError(`dstack RTMR3 event ${index} is not a runtime event`);
    }
    const digest = await sha384(preimage);
    if (!equal(digest, event.digest)) {
      throw new AciFormatError(`dstack runtime event ${index} digest does not match its fields`);
    }
    const input = new Uint8Array(96);
    input.set(mr);
    input.set(digest, 48);
    mr = await sha384(input);
  }
  return mr;
}

/** dstack-attest's find_event: first runtime match before system-ready. */
export function findDstackEvent(events: DstackEvent[], name: string): DstackEvent | undefined {
  for (const event of events) {
    if (event.imr !== 3 || event.event_type !== DSTACK_RUNTIME_EVENT_TYPE) continue;
    if (event.event === 'system-ready') break;
    if (event.event === name) return event;
  }
  return undefined;
}
