import assert from "node:assert/strict";
import { test } from "node:test";
import { newerState } from "../src/renderer/lib/use-app-state.ts";

const state = (backendInstance, sequence, status = "stopped") => ({ backendInstance, sequence, status });

test("an older state of the same backend never replaces a newer one", () => {
  const event = state("a", 5, "verifying");
  const commandResult = state("a", 4, "stopped");
  assert.equal(newerState(event, commandResult), event);
});

test("a state of the same sequence or later replaces the current one", () => {
  const current = state("a", 5);
  const same = state("a", 5, "verifying");
  const later = state("a", 6, "verified");
  assert.equal(newerState(current, same), same);
  assert.equal(newerState(current, later), later);
});

test("another backend's state replaces the current one whatever its sequence", () => {
  const current = state("a", 9);
  const restarted = state("b", 1);
  assert.equal(newerState(current, restarted), restarted);
});

test("the first state is taken as it is", () => {
  const first = state("a", 3);
  assert.equal(newerState(undefined, first), first);
});
