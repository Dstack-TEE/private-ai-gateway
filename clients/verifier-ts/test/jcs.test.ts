import { test } from 'node:test';
import assert from 'node:assert/strict';
import { canonicalize, AciFormatError } from '../src/index.js';

test('JCS sorts object keys by UTF-16 code unit', () => {
  assert.equal(canonicalize({ b: 1, a: 2, c: 3 }), '{"a":2,"b":1,"c":3}');
  // Uppercase sorts before lowercase (code point A=0x41 < a=0x61).
  assert.equal(canonicalize({ a: 1, A: 2 }), '{"A":2,"a":1}');
});

test('JCS sorts nested objects recursively and preserves array order', () => {
  assert.equal(
    canonicalize({ z: [{ y: 1, x: 2 }], a: 0 }),
    '{"a":0,"z":[{"x":2,"y":1}]}',
  );
});

test('JCS serializes integers, booleans, null, and strings', () => {
  assert.equal(
    canonicalize({ n: 1800000000, t: true, f: false, z: null, s: 'hi' }),
    '{"f":false,"n":1800000000,"s":"hi","t":true,"z":null}',
  );
  assert.equal(canonicalize(0), '0');
  assert.equal(canonicalize(-0), '0');
  assert.equal(canonicalize(-42), '-42');
});

test('JCS rejects non-integer numbers (ACI integer-only subset, §3)', () => {
  assert.throws(() => canonicalize({ x: 1.5 }), AciFormatError);
  assert.throws(() => canonicalize(Number.NaN), AciFormatError);
  assert.throws(() => canonicalize(Infinity), AciFormatError);
});

test('JCS drops undefined-valued members (build with explicit null instead)', () => {
  assert.equal(canonicalize({ a: 1, b: undefined, c: 3 }), '{"a":1,"c":3}');
  assert.equal(canonicalize({ a: 1, b: null }), '{"a":1,"b":null}');
});

test('JCS escapes strings per RFC 8785 (control chars, quote, backslash)', () => {
  assert.equal(canonicalize('a"b\\c'), '"a\\"b\\\\c"');
  assert.equal(canonicalize('\n\t'), '"\\n\\t"');
  assert.equal(canonicalize(String.fromCharCode(1)), '"\\u0001"');
});
