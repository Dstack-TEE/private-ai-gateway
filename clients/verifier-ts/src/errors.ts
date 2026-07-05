/**
 * Errors raised by the verifier for conditions that are *not* ordinary
 * verification failures. A failed check (bad signature, wrong hash) is reported
 * as `ok: false` in the result objects — never thrown — so callers cannot ignore
 * it by forgetting a try/catch. These errors mean "the input is malformed or the
 * algorithm is outside this verifier's Level 1 / Web Crypto scope".
 */

/** Base class for every error this package throws. */
export class AciError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'AciError';
  }
}

/** A JCS input violated the ACI subset (e.g. a non-integer number) or a hex/field value would not parse. */
export class AciFormatError extends AciError {
  constructor(message: string) {
    super(message);
    this.name = 'AciFormatError';
  }
}

/**
 * A signature or identity algorithm that ACI defines but this Web-Crypto-only
 * verifier cannot check. `ecdsa-secp256k1` is the expected case: the curve is
 * absent from the Web Crypto API, so verify it against the reference
 * implementation or a Level 2 verifier profile instead.
 */
export class UnsupportedAlgorithmError extends AciError {
  readonly algorithm: string;
  constructor(algorithm: string, context: string) {
    super(
      `unsupported algorithm "${algorithm}" for ${context}: this verifier supports only ed25519 via the Web Crypto API. ` +
        `secp256k1 is out of scope — verify it against the reference implementation or a Level 2 profile.`,
    );
    this.name = 'UnsupportedAlgorithmError';
    this.algorithm = algorithm;
  }
}
