import assert from "node:assert/strict";
import { test } from "node:test";

import { translateAciError, translateResponseErrors } from "../src/errors.ts";

test("upstream-2 evidence failures get an actionable, gateway-attributed message", () => {
  const error = new Error(
    "ACI receipt verification failed: NOT VERIFIED (1 fail: upstream-2; 5 pass)",
  );
  const translated = translateAciError(error, "phala");
  assert.notEqual(translated, error);
  assert.match(translated.message, /gateway-side/);
  assert.match(translated.message, /\/phala-receipt/);
  assert.match(translated.message, /Details: ACI receipt verification failed/);
  assert.equal(translated.cause, error);
  assert.equal(translated.name, "Error");
});

test("other receipt failures translate to a generic distrust message", () => {
  const translated = translateAciError(
    new Error(
      "ACI receipt verification failed: NOT VERIFIED (2 fails: receipt-1, receipt-4; 3 pass)",
    ),
    "phala",
  );
  assert.match(translated.message, /signed commitment/);
  assert.match(translated.message, /cannot be trusted/);
});

test("missing receipt and connection failures each get specific guidance", () => {
  const missing = translateAciError(
    new Error("successful ACI POST response carries no X-Receipt-Id"),
    "phala",
  );
  assert.match(missing.message, /without a receipt/);
  assert.match(missing.message, /gateway operator/);

  const connection = translateAciError(
    new Error(
      "[phala] inference blocked because no verified ACI connection is available: attestation failed",
    ),
    "phala",
  );
  assert.match(connection.message, /No verified ACI connection/);
  assert.match(connection.message, /\/phala-attestation/);

  const pin = translateAciError(new Error("ACI pinned request failed: channel_binding"), "phala");
  assert.match(pin.message, /TLS pin mismatch/);
});

test("unknown errors pass through unchanged", () => {
  const error = new Error("429: rate limit");
  assert.equal(translateAciError(error, "phala"), error);
  const wrapped = translateAciError("plain string", "phala");
  assert.match(wrapped.message, /plain string/);
});

test("translateResponseErrors forwards body bytes and translates mid-stream failures", async () => {
  let step = 0;
  const source = new ReadableStream<Uint8Array>({
    pull(controller) {
      if (step === 0) {
        step += 1;
        controller.enqueue(new TextEncoder().encode("hello "));
      } else if (step === 1) {
        step += 1;
        controller.enqueue(new TextEncoder().encode("world"));
      } else {
        controller.error(
          new Error("ACI receipt verification failed: NOT VERIFIED (1 fail: upstream-2; 5 pass)"),
        );
      }
    },
  });
  const response = new Response(source, { status: 200, headers: { "x-receipt-id": "rcpt-x" } });
  const wrapped = translateResponseErrors(response, "phala");

  const reader = wrapped.body!.getReader();
  let received = "";
  let failure: unknown;
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      received += new TextDecoder().decode(value);
    }
  } catch (error) {
    failure = error;
  }
  assert.equal(received, "hello world");
  assert.ok(failure instanceof Error);
  assert.match(failure.message, /gateway-side/);
  // The original auditor wording stays available in the details tail.
  assert.match(failure.message, /upstream-2/);
});

test("translateResponseErrors passes non-OK and bodiless responses through", async () => {
  const err = new Response("400 bad request", { status: 400 });
  assert.equal(translateResponseErrors(err, "phala"), err);

  const empty = new Response(null, { status: 200 });
  assert.equal(translateResponseErrors(empty, "phala"), empty);
});
