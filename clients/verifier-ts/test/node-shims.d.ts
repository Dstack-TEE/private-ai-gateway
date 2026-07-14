/**
 * Minimal ambient declarations for the Node test-runner surface the tests use.
 * Declared locally so the package needs only `typescript` as a devDependency —
 * no `@types/node`, which would drag Node globals into the DOM-typed src build.
 */

declare module 'node:test' {
  type TestFn = () => void | Promise<void>;
  export function test(name: string, fn: TestFn): Promise<void>;
  export function describe(name: string, fn: () => void): void;
  export function it(name: string, fn: TestFn): void;
}

declare module 'node:fs' {
  export function readFileSync(path: string | URL, encoding: 'utf8'): string;
}

declare module 'node:assert/strict' {
  interface AssertStrict {
    (value: unknown, message?: string): asserts value;
    ok(value: unknown, message?: string): asserts value;
    equal(actual: unknown, expected: unknown, message?: string): void;
    notEqual(actual: unknown, expected: unknown, message?: string): void;
    deepEqual(actual: unknown, expected: unknown, message?: string): void;
    notDeepEqual(actual: unknown, expected: unknown, message?: string): void;
    throws(fn: () => unknown, error?: unknown, message?: string): void;
    rejects(fn: () => Promise<unknown>, error?: unknown, message?: string): Promise<void>;
  }
  const assert: AssertStrict;
  export default assert;
}
