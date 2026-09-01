import assert from "node:assert/strict";
import test from "node:test";

import { buildTrayMenu } from "../src/main/tray-menu";
import type { GatewayState } from "../src/shared/contracts";

const actions = {
  copyEndpoint: () => undefined,
  openWindow: () => undefined,
  quit: () => undefined,
  start: () => undefined,
  stop: () => undefined,
};

function state(status: GatewayState["status"], proxyUrl?: string): GatewayState {
  return { status, proxyUrl, checks: [], activity: [] };
}

test("stopped tray menu offers start and disables endpoint copies", () => {
  const menu = buildTrayMenu(state("stopped"), actions);
  assert.equal(menu[1]?.label, "Status: Stopped");
  assert.equal(menu[4]?.label, "Start Gateway");
  assert.equal(menu[6]?.enabled, false);
  assert.equal(menu[7]?.enabled, false);
});

test("verified tray menu offers stop and endpoint copies", () => {
  const menu = buildTrayMenu(state("verified", "http://127.0.0.1:4180"), actions);
  assert.equal(menu[1]?.label, "Status: Verified");
  assert.equal(menu[4]?.label, "Stop Gateway");
  assert.equal(menu[6]?.enabled, true);
  assert.equal(menu[7]?.enabled, true);
});

test("verifying tray menu cannot start a second sidecar", () => {
  const menu = buildTrayMenu(state("verifying"), actions);
  assert.equal(menu[4]?.label, "Verifying...");
  assert.equal(menu[4]?.enabled, false);
});
