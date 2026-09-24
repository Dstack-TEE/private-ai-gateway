import { expect, test } from "bun:test";

import plugin, { RedPillProviderPlugin } from "../index.ts";

test("exposes OpenCode V1 and V2 entrypoints from one default export", () => {
  expect(plugin.id).toBe("opencode-provider-redpill");
  expect(typeof plugin.setup).toBe("function");
  expect(typeof plugin.server).toBe("function");
});

test("keeps the V1 server-plugin export", () => {
  expect(typeof RedPillProviderPlugin).toBe("function");
  expect(RedPillProviderPlugin).toBe(plugin.server);
});
