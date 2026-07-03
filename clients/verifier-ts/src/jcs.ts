/**
 * JSON Canonicalization Scheme (RFC 8785) for the ACI subset.
 *
 * ACI restricts JSON numbers to integers (§3), so this omits ECMAScript number
 * formatting and instead *rejects* non-integer numbers — a conformant ACI object
 * never contains one, and rejecting is safer than silently mis-serializing. For
 * strings we reuse `JSON.stringify`, whose escaping is exactly RFC 8785's
 * (minimal escapes, `\uXXXX` for other controls, lone surrogates escaped) since
 * ES2019. Object members are ordered by their UTF-16 code units, which is what
 * JavaScript's `<` on strings compares, and `undefined`-valued members are
 * dropped (standard JSON behaviour) — build with explicit `null` where a field
 * must appear.
 */

import { AciFormatError } from './errors.js';

/** A value canonicalizable under the ACI JCS subset. */
export type JcsValue =
  | string
  | number
  | boolean
  | null
  | JcsValue[]
  | { [key: string]: JcsValue | undefined };

/** Canonicalize a value to its RFC 8785 string form. */
export function canonicalize(value: JcsValue): string {
  if (value === null) return 'null';
  switch (typeof value) {
    case 'boolean':
      return value ? 'true' : 'false';
    case 'number':
      if (!Number.isInteger(value)) {
        throw new AciFormatError(
          `JCS: ACI restricts numbers to integers, got ${value} (§3)`,
        );
      }
      // -0 canonicalizes to "0".
      return Object.is(value, -0) ? '0' : String(value);
    case 'string':
      return JSON.stringify(value);
    case 'object':
      if (Array.isArray(value)) {
        return '[' + value.map(canonicalize).join(',') + ']';
      }
      return serializeObject(value);
    default:
      throw new AciFormatError(`JCS: unsupported type ${typeof value}`);
  }
}

function serializeObject(obj: { [key: string]: JcsValue | undefined }): string {
  const keys = Object.keys(obj)
    .filter((k) => obj[k] !== undefined)
    // RFC 8785 orders by UTF-16 code units; JS `<` on strings does exactly that.
    .sort((a, b) => (a < b ? -1 : a > b ? 1 : 0));
  let out = '{';
  for (let i = 0; i < keys.length; i++) {
    const k = keys[i]!;
    if (i > 0) out += ',';
    out += JSON.stringify(k) + ':' + canonicalize(obj[k] as JcsValue);
  }
  return out + '}';
}

/** Canonicalize and encode to UTF-8 bytes — the form fed to SHA-256 and signatures. */
export function jcsBytes(value: JcsValue): Uint8Array {
  return new TextEncoder().encode(canonicalize(value));
}
