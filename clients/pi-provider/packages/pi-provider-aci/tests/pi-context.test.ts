import assert from "node:assert/strict";
import { test } from "node:test";

import {
  contextForResolvedPi,
  transcriptToLegacyContext,
} from "../src/pi-context.ts";

const transcript = {
  messages: [
    {
      role: "system",
      content: "You are helpful",
      sections: { rules: "Be brief" },
      toolsAdded: [{ name: "bash", description: "run", parameters: { type: "object" } }],
    },
    { role: "user", content: "hi", timestamp: 1 },
  ],
};

test("legacy pi-ai receives the system prompt and tools outside the message list", () => {
  const legacy = transcriptToLegacyContext(transcript);
  assert.equal(legacy.systemPrompt, "You are helpful\n\nBe brief");
  assert.equal(legacy.tools?.length, 1);
  assert.equal(legacy.tools?.[0]?.name, "bash");
  assert.deepEqual(
    legacy.messages.map((message) => message.role),
    ["user"],
  );
});

test("a transcript-aware pi-ai keeps the host context untouched", () => {
  const aware = { getCurrentSystemPrompt() {} };
  assert.equal(contextForResolvedPi(aware, transcript), transcript);
});

test("an older pi-ai gets the folded context so streamSimple does not walk string content", () => {
  const folded = contextForResolvedPi({}, transcript) as ReturnType<typeof transcriptToLegacyContext>;
  assert.equal(folded.systemPrompt, "You are helpful\n\nBe brief");
  assert.equal(folded.messages.some((message) => message.role === "system"), false);
});
