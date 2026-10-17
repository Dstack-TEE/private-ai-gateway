import assert from "node:assert/strict";
import { test } from "node:test";
import { QueryClient } from "@tanstack/react-query";
import { newerState, readState } from "../src/renderer/lib/use-app-state.ts";

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

test("a disconnected state applies whatever its sequence, and a reconnected one replaces it", () => {
  const connected = state("a", 7, "verified");
  const disconnected = { ...state("a", 7, "error"), backendConnected: false };
  assert.equal(newerState(connected, disconnected), disconnected);
  const reconnected = state("a", 7, "verified");
  assert.equal(newerState(disconnected, reconnected), reconnected);
});

const pendingRead = () => {
  let resolve;
  const reply = new Promise((settle) => { resolve = settle; });
  return { api: { getState: () => reply }, resolve };
};

test("a newer state that arrives during a read survives the older reply", async () => {
  const client = new QueryClient();
  const { api, resolve } = pendingRead();
  const read = readState(client, api);
  const event = state("a", 6, "verifying");
  client.setQueryData(["app-state"], event);
  resolve(state("a", 5));
  assert.deepEqual(await read, event);
});

test("another backend's state that arrives during a read replaces the reply", async () => {
  const client = new QueryClient();
  client.setQueryData(["app-state"], state("a", 3));
  const { api, resolve } = pendingRead();
  const read = readState(client, api);
  const replacement = state("b", 1);
  client.setQueryData(["app-state"], replacement);
  resolve(state("a", 4));
  assert.deepEqual(await read, replacement);
});
