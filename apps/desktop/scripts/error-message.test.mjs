import assert from "node:assert/strict";
import { test } from "node:test";
import { errorMessage } from "../src/renderer/lib/error-message.ts";

test("a failed call's message says why, never empty", () => {
  assert.equal(errorMessage(new Error("Settings are read-only.")), "Settings are read-only.");
  assert.equal(errorMessage("No browser is configured."), "No browser is configured.");
  assert.equal(errorMessage(new Error("")), "The operation could not complete. Try again.");
  assert.equal(errorMessage(undefined), "The operation could not complete. Try again.");
});
