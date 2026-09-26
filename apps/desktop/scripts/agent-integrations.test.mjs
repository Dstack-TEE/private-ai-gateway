import assert from "node:assert/strict";
import { test } from "node:test";
import { MutationObserver, QueryClient, QueryObserver } from "@tanstack/react-query";
import { agentAccessMutation, agentIntegrationsLocked, completeAgentStatuses, readAgentIntegrations, supportedAgentStatuses } from "../src/renderer/lib/agent-integrations.ts";

function fixture(status, enabledStatus = status) {
  const calls = [];
  const agents = [{ id: "codex", installed: true, connected: false }];
  const api = {
    async getAgentAccess() { calls.push("restore"); return status; },
    async requestAgentAccess() { calls.push("enable"); return enabledStatus; },
    async listAgents() { calls.push("scan"); return agents; },
  };
  return { api, calls, agents };
}

test("inactive integrations never scan or request a panel during refresh", async () => {
  const { api, calls } = fixture("authorizationRequired");
  assert.deepEqual(await readAgentIntegrations(api, true), { accessStatus: "authorizationRequired", agents: supportedAgentStatuses() });
  assert.deepEqual(calls, ["restore"]);
});

test("explicit Enable continues with detection and does not connect an Agent", async () => {
  const { api, calls, agents } = fixture("authorizationRequired", "authorized");
  assert.deepEqual(await readAgentIntegrations(api, true, true), { accessStatus: "authorized", agents });
  assert.deepEqual(calls, ["enable", "scan"]);
});

test("cancel leaves integrations inactive without a scan or connection side effects", async () => {
  const { api, calls } = fixture("authorizationRequired");
  assert.deepEqual(await readAgentIntegrations(api, true, true), { accessStatus: "authorizationRequired", agents: supportedAgentStatuses() });
  assert.deepEqual(calls, ["enable"]);
});

test("restorable access scans silently on every refresh", async () => {
  const { api, calls, agents } = fixture("authorized");
  assert.deepEqual(await readAgentIntegrations(api, true), { accessStatus: "authorized", agents });
  await readAgentIntegrations(api, true);
  assert.deepEqual(calls, ["restore", "scan", "restore", "scan"]);
});

test("unrecoverable access clears detection without requesting a panel", async () => {
  const { api, calls } = fixture("reauthorizationRequired");
  assert.deepEqual(await readAgentIntegrations(api, true), { accessStatus: "reauthorizationRequired", agents: supportedAgentStatuses() });
  assert.deepEqual(calls, ["restore"]);
});

test("permission loss during a scan returns to inactive without a global failure", async () => {
  let checks = 0;
  const { api, calls } = fixture("authorized");
  api.getAgentAccess = async () => ++checks === 1 ? "authorized" : "reauthorizationRequired";
  api.listAgents = async () => { throw new Error("Access lost"); };
  assert.deepEqual(await readAgentIntegrations(api, true), { accessStatus: "reauthorizationRequired", agents: supportedAgentStatuses() });
  assert.deepEqual(calls, []);
});

test("Direct and non-macOS scan without calling authorization APIs", async () => {
  const { api, calls, agents } = fixture("authorizationRequired");
  assert.deepEqual(await readAgentIntegrations(api, false), { accessStatus: "authorized", agents });
  assert.deepEqual(calls, ["scan"]);
});

function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}

test("Enable scans once access is granted and publishes the result to every agents query before unlocking", async () => {
  const { api, calls, agents } = fixture("authorizationRequired");
  const access = deferred();
  const scan = deferred();
  api.requestAgentAccess = () => { calls.push("enable"); return access.promise; };
  api.listAgents = () => { calls.push("scan"); return scan.promise; };
  const client = new QueryClient();
  const key = ["agents", "backend", 1];
  client.setQueryData(key, { accessStatus: "authorizationRequired", agents: [] });
  const observed = [];
  const unsubscribe = new QueryObserver(client, { queryKey: key, enabled: false }).subscribe(({ data }) => observed.push(data));
  const mutation = new MutationObserver(client, agentAccessMutation(api, true, client));
  const locked = () => agentIntegrationsLocked(client.getQueryData(key)?.accessStatus, mutation.getCurrentResult().isPending);
  try {
    assert.equal(locked(), true);
    const run = mutation.mutate();
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(mutation.getCurrentResult().isPending, true);
    access.resolve("authorized");
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(calls, ["enable", "scan"]);
    assert.equal(locked(), true);
    scan.resolve(agents);
    await run;
    assert.equal(locked(), false);
    assert.deepEqual(client.getQueryData(key), { accessStatus: "authorized", agents });
    assert.deepEqual(observed.at(-1), { accessStatus: "authorized", agents });
    assert.deepEqual(calls, ["enable", "scan"]); // Enabling never connects.
  } finally { unsubscribe(); client.clear(); }
});

test("a cancelled Enable stays locked and can be retried; a failed scan also ends it", async () => {
  const { api, calls, agents } = fixture("authorizationRequired");
  const client = new QueryClient();
  const key = ["agents"];
  client.setQueryData(key, { accessStatus: "authorizationRequired", agents: [] });
  const mutation = new MutationObserver(client, agentAccessMutation(api, true, client));
  try {
    await mutation.mutate();
    assert.equal(agentIntegrationsLocked(client.getQueryData(key).accessStatus, mutation.getCurrentResult().isPending), true);
    assert.deepEqual(calls, ["enable"]);
    api.requestAgentAccess = async () => "authorized";
    api.getAgentAccess = async () => "authorized";
    api.listAgents = async () => { throw new Error("Scan unavailable"); };
    await assert.rejects(mutation.mutate(), /Scan unavailable/);
    assert.equal(mutation.getCurrentResult().isPending, false);
    assert.equal(agentIntegrationsLocked(client.getQueryData(key).accessStatus, false), true);
    api.listAgents = async () => agents;
    await mutation.mutate();
    assert.equal(agentIntegrationsLocked(client.getQueryData(key).accessStatus, false), false);
  } finally { client.clear(); }
});

test("without authorization (Direct, Windows, Linux) Enable scans without requesting access", async () => {
  const { api, calls, agents } = fixture("authorizationRequired");
  const client = new QueryClient();
  client.setQueryData(["agents"], { accessStatus: "authorized", agents: [] });
  try {
    await new MutationObserver(client, agentAccessMutation(api, false, client)).mutate();
    assert.deepEqual(calls, ["scan"]);
    assert.deepEqual(client.getQueryData(["agents"]), { accessStatus: "authorized", agents });
  } finally { client.clear(); }
});

test("connection gate covers missing access, revoked access, and the whole pending scan", () => {
  for (const status of [undefined, "authorizationRequired", "reauthorizationRequired"]) {
    assert.equal(agentIntegrationsLocked(status, false), true);
  }
  assert.equal(agentIntegrationsLocked("authorized", true), true);
  assert.equal(agentIntegrationsLocked("authorized", false), false);
});

test("partial detection still exposes every supported agent", () => {
  const listed = completeAgentStatuses([{ ...supportedAgentStatuses()[0], installed: true }]);
  assert.deepEqual(listed.map((agent) => agent.id), supportedAgentStatuses().map((agent) => agent.id));
  assert.equal(listed[0].installed, true);
  assert.equal(listed.at(-1)?.installed, false);
});
