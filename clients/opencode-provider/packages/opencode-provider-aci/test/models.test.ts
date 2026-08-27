import { expect, test } from "bun:test";

import { mapOpenCodeModel } from "../src/index.ts";

test("maps provider capabilities and Qwen thinking variants into OpenCode", () => {
  const model = mapOpenCodeModel({
    id: "qwen/qwen3-coder",
    name: "Qwen3 Coder",
    reasoning: true,
    thinkingFormat: "qwen",
    toolCall: true,
    temperature: true,
    input: ["text", "image"],
    output: ["text"],
    cost: { input: 0.2, output: 0.8, cacheRead: 0.1, cacheWrite: 0 },
    contextWindow: 262_144,
    maxOutputTokens: 65_536,
  });

  expect(model.reasoning).toBe(true);
  expect(model.attachment).toBe(true);
  expect(model.tool_call).toBe(true);
  expect(model.interleaved).toEqual({ field: "reasoning_content" });
  expect(model.limit).toEqual({ context: 262_144, output: 65_536 });
  expect(model.variants).toEqual({
    off: { enable_thinking: false },
    high: { enable_thinking: true },
  });
});
