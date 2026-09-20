import assert from "node:assert/strict";
import { test } from "node:test";
import { QueryClient, QueryObserver } from "@tanstack/react-query";
import { agentIntegrationsLocked, createAgentAccessAction, readAgentIntegrations, supportedAgentStatuses } from "../src/renderer/lib/agent-integrations.ts";

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

test("Enable serializes requests and publishes scanned results to shared observers before unlocking", async () => {
  const { api, calls, agents } = fixture("authorizationRequired");
  const access = deferred();
  const scan = deferred();
  api.requestAgentAccess = () => { calls.push("enable"); return access.promise; };
  api.listAgents = () => { calls.push("scan"); return scan.promise; };
  const client = new QueryClient();
  client.setQueryData(["agents"], { accessStatus: "authorizationRequired", agents: [] });
  const observed = [];
  const observer = new QueryObserver(client, { queryKey: ["agents"], enabled: false });
  const unsubscribe = observer.subscribe(({ data }) => observed.push(data));
  const pending = [];
  const action = createAgentAccessAction(api, true, client, (value) => pending.push(value));
  const locked = () => agentIntegrationsLocked(client.getQueryData(["agents"])?.accessStatus, action.pending);
  try {
    assert.equal(locked(), true);
    const first = action.run();
    await action.run();
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(action.pending, true);
    assert.deepEqual(calls, ["enable"]);
    access.resolve("authorized");
    await new Promise((resolve) => setImmediate(resolve));
    assert.deepEqual(calls, ["enable", "scan"]);
    assert.equal(locked(), true);
    assert.deepEqual(client.getQueryData(["agents"]).agents, []);
    scan.resolve(agents);
    await first;
    assert.equal(locked(), false);
    assert.deepEqual(client.getQueryData(["agents"]), { accessStatus: "authorized", agents });
    assert.deepEqual(observed.at(-1), { accessStatus: "authorized", agents });
    assert.deepEqual(pending, [true, false]);
    assert.deepEqual(calls, ["enable", "scan"]); // Enabling never connects.
  } finally { unsubscribe(); client.clear(); }
});

test("cancel remains locked and allows retry; failed scans also release the request guard", async () => {
  const { api, calls, agents } = fixture("authorizationRequired");
  const client = new QueryClient();
  const action = createAgentAccessAction(api, true, client, () => {});
  try {
    await action.run();
    assert.equal(action.pending, false);
    assert.equal(agentIntegrationsLocked(client.getQueryData(["agents"]).accessStatus, action.pending), true);
    assert.deepEqual(calls, ["enable"]);
    api.requestAgentAccess = async () => "authorized";
    api.getAgentAccess = async () => "authorized";
    api.listAgents = async () => { throw new Error("Scan unavailable"); };
    await assert.rejects(action.run(), /Scan unavailable/);
    assert.equal(action.pending, false);
    assert.equal(agentIntegrationsLocked(client.getQueryData(["agents"]).accessStatus, false), true);
    api.listAgents = async () => agents;
    await action.run();
    assert.equal(agentIntegrationsLocked(client.getQueryData(["agents"]).accessStatus, false), false);
  } finally { client.clear(); }
});

test("Direct / Windows / Linux actions never request access, including before the initial scan", async () => {
  const { api, calls } = fixture("authorizationRequired");
  const client = new QueryClient();
  try {
    const action = createAgentAccessAction(api, false, client, () => assert.fail("must not start authorization"));
    await action.run();
    assert.equal(action.pending, false);
    assert.deepEqual(calls, []);
    assert.equal(agentIntegrationsLocked("authorized", false), false);
  } finally { client.clear(); }
});

test("connection gate covers missing access, revoked access, and the whole pending scan", () => {
  for (const status of [undefined, "authorizationRequired", "reauthorizationRequired"]) {
    assert.equal(agentIntegrationsLocked(status, false), true);
  }
  assert.equal(agentIntegrationsLocked("authorized", true), true);
  assert.equal(agentIntegrationsLocked("authorized", false), false);
});
