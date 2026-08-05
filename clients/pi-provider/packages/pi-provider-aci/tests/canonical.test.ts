import assert from "node:assert/strict";
import { test } from "node:test";

import { canonicalize, CanonicalError, parseAciJson } from "../src/canonical.ts";

function canon(value: unknown): string {
  return new TextDecoder().decode(canonicalize(value as never));
}

test("canonicalize: object keys sorted by UTF-16 code unit", () => {
  const input = { b: 1, a: 2 };
  assert.equal(canon(input), '{"a":2,"b":1}');
});

test("canonicalize: nested objects preserve value order", () => {
  const input = { z: { b: 2, a: 1 }, a: 3 };
  assert.equal(canon(input), '{"a":3,"z":{"a":1,"b":2}}');
});

test("canonicalize: arrays keep declared order", () => {
  const input = [3, 1, 2];
  assert.equal(canon(input), '[3,1,2]');
});

test("canonicalize: escapes control characters", () => {
  const input = { text: 'line1\nline2\t"quoted"' };
  assert.equal(canon(input), '{"text":"line1\\nline2\\t\\"quoted\\""}');
});

test("canonicalize: rejects floats", () => {
  assert.throws(() => canon({ value: 1.5 }), CanonicalError);
});

test("canonicalize: accepts large integers", () => {
  assert.equal(canon({ value: 9007199254740991 }), '{"value":9007199254740991}');
});

test("canonicalize: null, true, false", () => {
  assert.equal(canon({ a: null, b: true, c: false }), '{"a":null,"b":true,"c":false}');
});

test("canonicalize: bigint renders as integer string", () => {
  assert.equal(canon({ value: 18446744073709551615n }), '{"value":18446744073709551615}');
});

test("parseAciJson: preserves u64 integers beyond safe range as bigint", () => {
  const parsed = parseAciJson('{"not_after":18446744073709551615,"version":1}') as Record<string, unknown>;
  assert.equal(typeof parsed.not_after, "bigint");
  assert.equal(parsed.not_after, 18446744073709551615n);
  assert.equal(typeof parsed.version, "number");
  assert.equal(parsed.version, 1);
});

test("parseAciJson: leaves strings containing digits unchanged", () => {
  const parsed = parseAciJson('{"id":"18446744073709551615"}') as Record<string, unknown>;
  assert.equal(parsed.id, "18446744073709551615");
});
