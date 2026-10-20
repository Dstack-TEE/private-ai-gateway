import assert from "node:assert/strict";
import { test } from "node:test";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { checkForUpdatesNow } from "../src/renderer/lib/update-check.ts";

const queryKey = ["app-update"];

/** An update query with one observer, as the window has, whose checks finish when the test says. */
function updateQuery({ enabled = true } = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const checks = [];
  const queryFn = () => new Promise((resolve) => checks.push(resolve));
  const observer = new QueryObserver(client, { queryKey, queryFn, enabled, staleTime: Infinity });
  const unsubscribe = observer.subscribe(() => {});
  return { client, checks, unsubscribe };
}

test("overlapping requests to check for updates join the check under way", async () => {
  const { client, checks, unsubscribe } = updateQuery();
  // The window's own first check finished.
  checks[0]({ version: null });
  await client.getQueryCache().find({ queryKey }).promise;
  // A check that replaced one under way would run a second update command,
  // which fails while the first holds the shell's update operation.
  const requests = [checkForUpdatesNow(client, queryKey), checkForUpdatesNow(client, queryKey)];
  assert.equal(checks.length, 2);
  checks[1]({ version: "1.2.3" });
  await Promise.all(requests);
  assert.deepEqual(client.getQueryData(queryKey), { version: "1.2.3" });
  // Once it finished, a request checks again.
  const next = checkForUpdatesNow(client, queryKey);
  assert.equal(checks.length, 3);
  checks[2]({ version: null });
  await next;
  assert.deepEqual(client.getQueryData(queryKey), { version: null });
  unsubscribe();
});

test("a request does not check while the check is disabled", async () => {
  const { client, checks, unsubscribe } = updateQuery({ enabled: false });
  await checkForUpdatesNow(client, queryKey);
  assert.equal(checks.length, 0);
  unsubscribe();
});
