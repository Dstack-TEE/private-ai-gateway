// Canonical JSON (JCS, RFC 8785 subset) matching the gateway's
// `src/aci/canonical.rs`. Used for receipt signature verification,
// attestation report binding, workload id/keyset digest recomputation.
//
// Subset implemented (matches ACI wire shapes):
//   - null, bool, integer (i64/u64), string, array (declared order), object
//   - object keys sorted by UTF-16 code unit sequence (RFC 8785 §3.2.3)
//   - strings escaped per RFC 8785 §3.2.2.2; non-ASCII emitted as UTF-8 bytes
//   - floats REJECTED (ACI defines only integer numerics; a conformant ACI
//     object never contains a float, so this never fires on valid input)
//
// This is NOT a general RFC 8785 implementation; it covers exactly what ACI
// protocol objects need. Mirrors the Rust implementation's documented scope.

export class CanonicalError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "CanonicalError";
  }
}

export type Json =
  | null
  | boolean
  | number
  | bigint
  | string
  | Json[]
  | { [key: string]: Json }
  | unknown;

export function canonicalize(value: Json): Uint8Array {
  const chunks: Uint8Array[] = [];
  writeValue(chunks, value);
  const total = chunks.reduce((n, c) => n + c.length, 0);
  const out = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    out.set(chunk, offset);
    offset += chunk.length;
  }
  return out;
}

export function canonicalizeString(value: Json): string {
  return new TextDecoder().decode(canonicalize(value));
}

function writeValue(chunks: Uint8Array[], value: Json): void {
  if (value === null) {
    chunks.push(ASCII("null"));
    return;
  }
  if (typeof value === "boolean") {
    chunks.push(ASCII(value ? "true" : "false"));
    return;
  }
  if (typeof value === "bigint") {
    chunks.push(ASCII(value.toString()));
    return;
  }
  if (typeof value === "number") {
    writeNumber(chunks, value);
    return;
  }
  if (typeof value === "string") {
    writeString(chunks, value);
    return;
  }
  if (Array.isArray(value)) {
    chunks.push(ASCII("["));
    for (let i = 0; i < value.length; i++) {
      if (i > 0) chunks.push(ASCII(","));
      writeValue(chunks, value[i] as Json);
    }
    chunks.push(ASCII("]"));
    return;
  }
  writeObject(chunks, value as { [key: string]: Json });
}

function writeNumber(chunks: Uint8Array[], n: number): void {
  if (!Number.isInteger(n)) {
    throw new CanonicalError(
      "JCS: float / non-integer numeric is not allowed in the ACI value space",
    );
  }
  if (!Number.isSafeInteger(n)) {
    throw new CanonicalError(
      "JCS: integer outside safe range must be supplied as bigint, not number",
    );
  }
  chunks.push(ASCII(String(n)));
}

function writeObject(chunks: Uint8Array[], obj: { [key: string]: Json }): void {
  const keys = Object.keys(obj);
  keys.sort(utf16Compare);
  chunks.push(ASCII("{"));
  for (let i = 0; i < keys.length; i++) {
    if (i > 0) chunks.push(ASCII(","));
    writeString(chunks, keys[i]);
    chunks.push(ASCII(":"));
    writeValue(chunks, obj[keys[i]]);
  }
  chunks.push(ASCII("}"));
}

// Lexicographic compare on the UTF-16 code unit sequence. For BMP this
// matches String comparison; for supplementary-plane chars it does not.
function utf16Compare(a: string, b: string): number {
  const len = Math.min(a.length, b.length);
  for (let i = 0; i < len; i++) {
    const ca = a.charCodeAt(i);
    const cb = b.charCodeAt(i);
    if (ca !== cb) return ca - cb;
  }
  return a.length - b.length;
}

function writeString(chunks: Uint8Array[], s: string): void {
  chunks.push(ASCII('"'));
  for (const c of s) {
    const code = c.codePointAt(0);
    if (c === '"') chunks.push(ASCII('\\"'));
    else if (c === "\\") chunks.push(ASCII("\\\\"));
    else if (c === "\b") chunks.push(ASCII("\\b"));
    else if (c === "\t") chunks.push(ASCII("\\t"));
    else if (c === "\n") chunks.push(ASCII("\\n"));
    else if (c === "\f") chunks.push(ASCII("\\f"));
    else if (c === "\r") chunks.push(ASCII("\\r"));
    else if (code !== undefined && code < 0x20) {
      chunks.push(ASCII(`\\u${code.toString(16).padStart(4, "0")}`));
    } else {
      chunks.push(new TextEncoder().encode(c));
    }
  }
  chunks.push(ASCII('"'));
}

function ASCII(s: string): Uint8Array {
  // All ASCII control escape outputs (`\\n` etc.) are ASCII; TextEncoder is
  // safe for arbitrary strings and produces UTF-8 for non-ASCII.
  return new TextEncoder().encode(s);
}

const ACI_BIGINT_PREFIX = "__aci_bigint__:";

/**
 * Parse a JSON text preserving integers that are outside the IEEE-754 safe
 * integer range as `bigint`. ACI uses `u64`/`i64` for timestamps and epoch
 * versions; JavaScript's `JSON.parse` rounds them, which breaks digest
 * recomputation. Floats and scientific-notation numbers are left untouched.
 */
export function parseAciJson(text: string): unknown {
  const transformed = transformLargeIntegers(text);
  return JSON.parse(transformed, (_key, value: unknown) => {
    if (typeof value === "string" && value.startsWith(ACI_BIGINT_PREFIX)) {
      return BigInt(value.slice(ACI_BIGINT_PREFIX.length));
    }
    return value;
  });
}

function transformLargeIntegers(text: string): string {
  let out = "";
  let i = 0;
  let inString = false;
  while (i < text.length) {
    const c = text[i];
    if (inString) {
      if (c === "\\") {
        out += c;
        i++;
        if (i < text.length) {
          out += text[i];
          i++;
        }
        continue;
      }
      if (c === '"') inString = false;
      out += c;
      i++;
      continue;
    }
    if (c === '"') {
      inString = true;
      out += c;
      i++;
      continue;
    }
    if (c === "-" || isDigit(c)) {
      const start = i;
      if (c === "-") i++;
      while (i < text.length && isDigit(text[i])) i++;
      if (i < text.length && text[i] === ".") {
        i++;
        while (i < text.length && isDigit(text[i])) i++;
      }
      if (i < text.length && (text[i] === "e" || text[i] === "E")) {
        i++;
        if (i < text.length && (text[i] === "+" || text[i] === "-")) i++;
        while (i < text.length && isDigit(text[i])) i++;
      }
      const token = text.slice(start, i);
      // Only rewrite plain integers that exceed the safe integer range.
      if (!token.includes(".") && !token.toLowerCase().includes("e")) {
        const n = BigInt(token);
        if (n > Number.MAX_SAFE_INTEGER || n < Number.MIN_SAFE_INTEGER) {
          out += `"${ACI_BIGINT_PREFIX}${token}"`;
          continue;
        }
      }
      out += token;
      continue;
    }
    out += c;
    i++;
  }
  return out;
}

function isDigit(c: string): boolean {
  return c >= "0" && c <= "9";
}
