import { expect, test } from "bun:test";

import plugin, { RedPillProviderPluginV2 } from "../index.ts";

test("exposes OpenCode V1 and V2 entrypoints from one default export", () => {
  expect(plugin.id).toBe("opencode-provider-redpill");
  expect(typeof plugin.setup).toBe("function");
  expect(typeof plugin.server).toBe("function");
  expect(RedPillProviderPluginV2.id).toBe("opencode-provider-redpill");
  expect(typeof RedPillProviderPluginV2.setup).toBe("function");
});
