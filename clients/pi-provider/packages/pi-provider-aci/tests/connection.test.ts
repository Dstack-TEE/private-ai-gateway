import assert from "node:assert/strict";
import { test } from "node:test";

import {
  ensureAciConnection,
  type AciConnectionState,
  type ConnectableAciProvider,
} from "../src/connection.ts";

class DeferredProvider implements ConnectableAciProvider {
  private readonly ready: Promise<void>;

  constructor(ready: Promise<void>) {
    this.ready = ready;
  }

  connect(): Promise<void> {
    return this.ready;
  }

  close(): Promise<void> {
    return Promise.resolve();
  }
}

test("connection completion publishes the verified footer state", async () => {
  let finishConnection: (() => void) | undefined;
  const ready = new Promise<void>((resolve) => {
    finishConnection = resolve;
  });
  const phases: string[] = [];
  const state: AciConnectionState<DeferredProvider> = {
    profile: { logPrefix: "[test]" },
    config: { baseUrl: "https://gateway.example/v1", trust: {} },
    provider: undefined,
    providerConfigKey: undefined,
    connectionSetup: undefined,
    connectionError: undefined,
    renderConnectionStatus: () => {
      phases.push(state.provider ? "verified" : state.connectionError ? "blocked" : "pending");
    },
  };

  const connection = ensureAciConnection(state, () => new DeferredProvider(ready));
  await Promise.resolve();
  assert.deepEqual(phases, ["pending"]);

  finishConnection?.();
  await connection;
  assert.deepEqual(phases, ["pending", "verified"]);
});
