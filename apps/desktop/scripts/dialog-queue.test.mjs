import assert from "node:assert/strict";
import { test } from "node:test";
import { createDialogQueue } from "../src/renderer/lib/dialog-queue.ts";

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

test("a close while a request is shown never replaces it", async () => {
  const { screen, queue } = dialog();
  const first = queue.ask("Reset settings?");
  const second = queue.ask("Could not open the link");
  // A stray close report while the question is still on screen.
  assert.equal(queue.closed(), true);
  assert.deepEqual(screen.shown, ["Reset settings?"]);
  queue.answer(true);
  assert.equal(await first, true);
  assert.equal(queue.closed(), true);
  queue.answer(true);
  assert.equal(await second, true);
  assert.equal(queue.closed(), false);
});
