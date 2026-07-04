import assert from "node:assert/strict";
import { test } from "node:test";

import { gcm } from "@noble/ciphers/aes";
import { x25519 } from "@noble/curves/ed25519";
import { secp256k1 } from "@noble/curves/secp256k1";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import { bytesToHex, hexToBytes, utf8ToBytes } from "@noble/hashes/utils";

import {
  ALGO_SECP256K1,
  ALGO_X25519,
  encryptRequestField,
  generateNonce,
  requestAad,
  responseAad,
  sealSecp256k1,
  sealX25519,
} from "../src/index.ts";

// spec/test-vectors.md §7 — byte-exact expected AAD.
const REQUEST_AAD_VECTOR =
  '{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"messages.0.content","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.request.v2","ts":1750000000}';
const RESPONSE_AAD_VECTOR =
  '{"algo":"x25519-aes-256-gcm-hkdf-sha256","field":"choices.0.message.content","id":"chatcmpl-123","model":"demo-model","nonce":"000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f","purpose":"aci.e2ee.response.v2","ts":1750000000}';

// Deterministic known-answer ciphertexts produced by the Rust client
// (clients/rust/aci-e2ee) with the same fixed inputs. Matching them proves the
// two implementations interoperate byte-for-byte.
const KAT_X25519 =
  "a4e09292b651c278b9772c569f5fa9bb13d906b46ab68c9df9dc2b4409f8a209000102030405060708090a0beb61256ee060a4f0f13144b6b54211955b1aefeebd";
const KAT_SECP256K1 =
  "041b84c5567b126440995d3ed5aaba0565d71e1834604819ff9c17f5e9d5dd078f70beaf8f588b541507fed6a642c5ab42dfdf8120a7f639de5122d47a69a8e8d1000102030405060708090a0bc1efd31f5d132d09f59283db7d4c457c294b402312";

const EPH_SECRET = new Uint8Array(32).fill(1);
const RECIPIENT_SECRET = new Uint8Array(32).fill(2);
const GCM_NONCE = Uint8Array.from({ length: 12 }, (_, i) => i);
const KAT_MODEL = "demo-model";
const KAT_FIELD = "messages.0.content";
const KAT_NONCE = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const KAT_TS = 1750000000;
const KAT_PLAINTEXT = utf8ToBytes("hello");

const decoder = new TextDecoder();

function openX25519(recipientSecret: Uint8Array, blobHex: string, aad: Uint8Array): Uint8Array {
  const blob = hexToBytes(blobHex);
  const eph = blob.slice(0, 32);
  const nonce = blob.slice(32, 44);
  const ct = blob.slice(44);
  const shared = x25519.getSharedSecret(recipientSecret, eph);
  const key = hkdf(sha256, shared, new Uint8Array(0), utf8ToBytes("aci.e2ee.v2.x25519"), 32);
  return gcm(key, nonce, aad).decrypt(ct);
}

function openSecp256k1(recipientSecret: Uint8Array, blobHex: string, aad: Uint8Array): Uint8Array {
  const blob = hexToBytes(blobHex);
  const eph = blob.slice(0, 65);
  const nonce = blob.slice(65, 77);
  const ct = blob.slice(77);
  const sharedX = secp256k1.getSharedSecret(recipientSecret, eph, true).slice(1);
  const key = hkdf(sha256, sharedX, new Uint8Array(0), utf8ToBytes("aci.e2ee.v2.secp256k1"), 32);
  return gcm(key, nonce, aad).decrypt(ct);
}

test("request AAD matches spec vector", () => {
  const aad = requestAad(ALGO_X25519, "demo-model", "messages.0.content", KAT_NONCE, 1750000000);
  assert.equal(decoder.decode(aad), REQUEST_AAD_VECTOR);
});

test("response AAD matches spec vector", () => {
  const aad = responseAad(
    ALGO_X25519,
    "demo-model",
    "chatcmpl-123",
    "choices.0.message.content",
    KAT_NONCE,
    1750000000,
  );
  assert.equal(decoder.decode(aad), RESPONSE_AAD_VECTOR);
});

test("x25519 round trip", () => {
  const recipient = bytesToHex(x25519.getPublicKey(RECIPIENT_SECRET));
  const field = "messages.0.content";
  const blob = encryptRequestField(
    recipient,
    ALGO_X25519,
    "gpt-x",
    field,
    "nonce-abc",
    1700000000,
    utf8ToBytes("secret prompt"),
  );
  const aad = requestAad(ALGO_X25519, "gpt-x", field, "nonce-abc", 1700000000);
  assert.deepEqual(openX25519(RECIPIENT_SECRET, blob, aad), utf8ToBytes("secret prompt"));
});

test("secp256k1 round trip", () => {
  const recipient = bytesToHex(secp256k1.getPublicKey(RECIPIENT_SECRET, false));
  const field = "prompt";
  const blob = encryptRequestField(recipient, ALGO_SECP256K1, "gpt-x", field, "nonce-xyz", 42, utf8ToBytes("hi"));
  const aad = requestAad(ALGO_SECP256K1, "gpt-x", field, "nonce-xyz", 42);
  assert.deepEqual(openSecp256k1(RECIPIENT_SECRET, blob, aad), utf8ToBytes("hi"));
});

test("x25519 known-answer matches the Rust client", () => {
  const recipient = bytesToHex(x25519.getPublicKey(RECIPIENT_SECRET));
  const aad = requestAad(ALGO_X25519, KAT_MODEL, KAT_FIELD, KAT_NONCE, KAT_TS);
  const blob = sealX25519(recipient, EPH_SECRET, GCM_NONCE, KAT_PLAINTEXT, aad);
  assert.equal(blob, KAT_X25519);
});

test("secp256k1 known-answer matches the Rust client", () => {
  const recipient = bytesToHex(secp256k1.getPublicKey(RECIPIENT_SECRET, false));
  const aad = requestAad(ALGO_SECP256K1, KAT_MODEL, KAT_FIELD, KAT_NONCE, KAT_TS);
  const blob = sealSecp256k1(recipient, EPH_SECRET, GCM_NONCE, KAT_PLAINTEXT, aad);
  assert.equal(blob, KAT_SECP256K1);
});

test("generateNonce produces a fresh 64-lowercase-hex value", () => {
  const nonce = generateNonce();
  assert.match(nonce, /^[0-9a-f]{64}$/);
  assert.notEqual(generateNonce(), nonce);
});
