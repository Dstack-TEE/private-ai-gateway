import { expect, test } from "bun:test";

import { mapOpenCodeModel } from "../src/index.ts";

test("maps only catalog-declared capabilities into OpenCode", () => {
  const model = mapOpenCodeModel({
    id: "provider/model",
    name: "Provider Model",
    reasoning: true,
    toolCall: true,
    temperature: true,
    input: ["text", "image"],
    output: ["text"],
    cost: { input: 0.2, output: 0.8 },
    contextWindow: 262_144,
    maxOutputTokens: 65_536,
  });

  expect(model.reasoning).toBe(true);
  expect(model.attachment).toBe(true);
  expect(model.tool_call).toBe(true);
  expect(model.cost).toEqual({ input: 0.2, output: 0.8 });
  expect(model.limit).toEqual({ context: 262_144, output: 65_536 });
});
