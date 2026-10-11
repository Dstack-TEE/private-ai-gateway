import assert from "node:assert/strict";
import { test } from "node:test";
import { AuthoredError, errorMessage } from "../src/renderer/lib/error-message.ts";

const fallback = "The operation could not complete. Try again.";

test("an authored message says why", () => {
  assert.equal(errorMessage(new AuthoredError("Settings are read-only.")), "Settings are read-only.");
});

test("any other failure shows the fallback, never raw text or nothing", () => {
  assert.equal(errorMessage(new TypeError("Failed to fetch")), fallback);
  assert.equal(errorMessage(new Error("invalid args `profileId` for command `activate_profile`")), fallback);
  assert.equal(errorMessage("No browser is configured."), fallback);
  assert.equal(errorMessage(new AuthoredError("")), fallback);
  assert.equal(errorMessage(undefined), fallback);
});
