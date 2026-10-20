import assert from "node:assert/strict";
import { test } from "node:test";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { checkForUpdatesNow, latestUpdateCheck } from "../src/renderer/lib/update-check.ts";

const queryKey = ["app-update"];

/** An update query with one observer, as the window has, whose checks finish when the test says. */
function updateQuery({ enabled = true } = {}) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const checks = [];
  const queryFn = () => new Promise((resolve) => checks.push(resolve));
  const observer = new QueryObserver(client, { queryKey, queryFn, enabled, staleTime: Infinity });
  const unsubscribe = observer.subscribe(() => {});
  return { client, checks, queryFn, observer, unsubscribe };
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

test("Restart to Update takes the background check under way, and checks itself while checks are disabled", async () => {
  const { client, checks, queryFn, observer, unsubscribe } = updateQuery();
  checks[0]({ version: null });
  await client.getQueryCache().find({ queryKey }).promise;
  // A background check is under way when the restart asks for the latest release.
  void checkForUpdatesNow(client, queryKey);
  const latest = latestUpdateCheck(client, { queryKey, queryFn });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(checks.length, 2);
  checks[1]({ version: "1.2.3" });
  assert.deepEqual(await latest, { version: "1.2.3" });
  // The restart disables the window's checks; it still checks for the latest release.
  observer.setOptions({ queryKey, queryFn, enabled: false, staleTime: Infinity });
  const fresh = latestUpdateCheck(client, { queryKey, queryFn });
  assert.equal(checks.length, 3);
  checks[2]({ version: "1.2.4" });
  assert.deepEqual(await fresh, { version: "1.2.4" });
  unsubscribe();
});
