import assert from "node:assert/strict";
import { test } from "node:test";
import { failureAlert, presentAlert } from "../src/renderer/lib/alert.ts";

const alert = { title: "Could not stop protection", message: "The background service is not running." };

function surfaces({ sheetFails = false } = {}) {
  const shown = [];
  return {
    shown,
    tell: async (next) => {
      if (sheetFails) throw new Error("Invalid alert");
      shown.push(["sheet", next]);
    },
    inWindow: async (next) => { shown.push(["window", next]); },
  };
}

test("the macOS app reports a failure in the system sheet only", async () => {
  const { shown, tell, inWindow } = surfaces();
  await presentAlert(alert, tell, inWindow);
  assert.deepEqual(shown, [["sheet", alert]]);
});

test("without a system sheet a failure shows in the window's dialog", async () => {
  const { shown, inWindow } = surfaces();
  await presentAlert(alert, undefined, inWindow);
  assert.deepEqual(shown, [["window", alert]]);
});

test("a failure the system sheet could not show still shows in the window", async (t) => {
  t.mock.method(console, "error", () => undefined);
  const { shown, tell, inWindow } = surfaces({ sheetFails: true });
  await presentAlert(alert, tell, inWindow);
  assert.deepEqual(shown, [["window", alert]]);
});

test("a failure alert names what failed and says why, never with an empty message", () => {
  assert.deepEqual(failureAlert("Could not reset settings", new Error("Settings are read-only.")), { title: "Could not reset settings", message: "Settings are read-only." });
  assert.deepEqual(failureAlert("Could not open the link", "No browser is configured."), { title: "Could not open the link", message: "No browser is configured." });
  assert.equal(failureAlert("Could not export diagnostics", new Error("")).message, "The operation could not complete. Try again.");
});
