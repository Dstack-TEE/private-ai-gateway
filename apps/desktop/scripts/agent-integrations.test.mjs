import assert from "node:assert/strict";
import { test } from "node:test";
import { agentIntegrationsLocked, completeAgentStatuses, readAgentIntegrations, supportedAgentStatuses } from "../src/renderer/lib/agent-integrations.ts";

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
