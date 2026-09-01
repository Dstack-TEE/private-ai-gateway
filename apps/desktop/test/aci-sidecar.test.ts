import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { AciSidecar } from "../src/main/aci-sidecar";

const fixture = path.join(path.dirname(fileURLToPath(import.meta.url)), "mock-aci.mjs");

test("starts from a valid ready event and reads receipt metadata", async () => {
  const sidecar = new AciSidecar({ executablePath: fixture, startupTimeoutMs: 3_000 });
  try {
    const state = await sidecar.start({
      remoteUrl: "https://tee.redpill.ai/",
      requireProductionOs: false,
    });
    assert.equal(state.status, "verified");
    assert.equal(state.remoteUrl, "https://tee.redpill.ai");
    assert.equal(state.identity?.teeType, "tdx");
    assert.equal(state.checks.length, 3);

    const receipts = await sidecar.listReceipts();
    assert.equal(receipts.length, 1);
    assert.equal(receipts[0]?.receiptId, "rcpt-desktop-smoke-0001");
    assert.equal(receipts[0]?.verified, true);
  } finally {
    const stopped = await sidecar.stop();
    assert.equal(stopped.status, "stopped");
  }
});

test("surfaces a fail-closed block and a verified identity refresh", async () => {
  const sidecar = new AciSidecar({
    executablePath: fixture,
    env: { ...process.env, MOCK_ACI_RUNTIME_UPDATE: "1" },
    startupTimeoutMs: 3_000,
  });
  const statuses: string[] = [];
  const unsubscribe = sidecar.subscribe((state) => statuses.push(state.status));
  try {
    await sidecar.start({ remoteUrl: "https://tee.redpill.ai", requireProductionOs: false });
    await new Promise((resolve) => setTimeout(resolve, 220));
    assert.ok(statuses.includes("blocked"));
    assert.equal(sidecar.getState().status, "verified");
    assert.equal(sidecar.getState().identity?.keysetDigest, `sha256:${"d".repeat(64)}`);
  } finally {
    unsubscribe();
    await sidecar.stop();
  }
});
