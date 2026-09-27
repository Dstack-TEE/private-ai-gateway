import assert from "node:assert/strict";
import { test } from "node:test";
import { createDialogQueue, failureAlert, presentAlert } from "../src/renderer/lib/alert.ts";

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

function dialog() {
  const screen = { shown: [], open: false };
  const queue = createDialogQueue((request) => { screen.shown.push(request); screen.open = true; }, () => { screen.open = false; });
  return { screen, queue };
}

test("an alert raised while a question is open waits, and never answers the question", async () => {
  const { screen, queue } = dialog();
  const quit = queue.ask("Stop all and quit?");
  const failure = queue.ask("Could not stop protection");
  assert.deepEqual(screen.shown, ["Stop all and quit?"]);
  queue.answer(true);
  assert.equal(await quit, true);
  // The next request shows only once the answered dialog has closed.
  assert.deepEqual(screen.shown, ["Stop all and quit?"]);
  queue.closed();
  assert.deepEqual(screen.shown, ["Stop all and quit?", "Could not stop protection"]);
  queue.answer(true);
  queue.closed();
  assert.equal(await failure, true);
  assert.equal(screen.open, false);
});

test("a question asked while an alert is unread shows after it, in order", async () => {
  const { screen, queue } = dialog();
  const answers = [queue.ask("Could not reset settings"), queue.ask("Reset settings?"), queue.ask("Restart to update?")];
  for (const confirmed of [true, false, true]) {
    queue.answer(confirmed);
    queue.closed();
  }
  assert.deepEqual(screen.shown, ["Could not reset settings", "Reset settings?", "Restart to update?"]);
  assert.deepEqual(await Promise.all(answers), [true, false, true]);
});

test("closing the dialog after its answer never answers the next request", async () => {
  const { screen, queue } = dialog();
  const first = queue.ask("Delete profile?");
  const second = queue.ask("Could not delete the profile");
  // An action button answers, then the dialog reports it is closing (Cancel).
  queue.answer(true);
  queue.answer(false);
  queue.closed();
  assert.equal(await first, true);
  assert.deepEqual(screen.shown, ["Delete profile?", "Could not delete the profile"]);
  queue.answer(true);
  assert.equal(await second, true);
});
