/**
 * Errors raised by the verifier for conditions that are *not* ordinary
 * verification failures. A failed check (bad signature, wrong hash) is reported
 * as `ok: false` in the result objects — never thrown — so callers cannot ignore
 * it by forgetting a try/catch. These errors mean "the input is malformed".
 */

/** Base class for every error this package throws. */
export class AciError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'AciError';
  }
}

/** An input value would not parse (hex, base64, JSON) or violates a spec-pinned format. */
export class AciFormatError extends AciError {
  constructor(message: string) {
    super(message);
    this.name = 'AciFormatError';
  }
}
