import assert from "node:assert/strict";
import { test } from "node:test";

import { sha256 } from "@noble/hashes/sha2.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { generateKeyPairSync, sign as nodeSign } from "node:crypto";

import {
  sha256Hex,
  verifyReceiptSignatureEd25519,
} from "../src/crypto.ts";

test("sha256Hex matches gateway format", () => {
  const payload = new TextEncoder().encode("hello");
  const expected = `sha256:${bytesToHex(sha256(payload))}`;
  assert.equal(sha256Hex(payload), expected);
});

test("verifyReceiptSignatureEd25519: accepts valid RFC 8032 signature over canonical bytes", () => {
  const { publicKey, privateKey } = generateKeyPairSync("ed25519");
  const pubRaw = Buffer.from(publicKey.export({ format: "jwk" }).x as string, "base64url");
  assert.equal(pubRaw.length, 32);
  const msg = new TextEncoder().encode("canonical-receipt-bytes");
  const sig = nodeSign(null, msg, privateKey);
  assert.equal(verifyReceiptSignatureEd25519(pubRaw.toString("hex"), msg, sig), true);
  // Different message -> false.
  assert.equal(
    verifyReceiptSignatureEd25519(pubRaw.toString("hex"), new TextEncoder().encode("tampered"), sig),
    false,
  );
  // Wrong-length signature -> false.
  assert.equal(verifyReceiptSignatureEd25519(pubRaw.toString("hex"), msg, new Uint8Array(63)), false);
});