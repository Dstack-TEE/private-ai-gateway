import { expect, test } from "bun:test";

import plugin from "../index.ts";

test("exposes OpenCode V1 and V2 entrypoints from one default export", () => {
  expect(plugin.id).toBe("opencode-provider-redpill");
  expect(typeof plugin.setup).toBe("function");
  expect(typeof plugin.server).toBe("function");
});
