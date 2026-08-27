import assert from "node:assert/strict";
import test from "node:test";

import { resolveAciProviderConfig } from "../src/config.ts";
import { discoverAciModelCatalog, mapAciModel } from "../src/models.ts";
import { resolveAciProviderProfile } from "../src/profile.ts";

const profile = resolveAciProviderProfile({ defaultBaseURL: "https://gateway.example/v1" });
const config = resolveAciProviderConfig(profile, {
  models: { isTeeOnly: true, thinkingFormat: "auto", allowlist: ["qwen/qwen3-coder"] },
});

test("maps an ACI catalog entry into framework-neutral provider capabilities", () => {
  const model = mapAciModel(
    {
      id: "qwen/qwen3-coder",
      name: "Qwen3 Coder",
      is_tee: true,
      context_length: 262_144,
      max_output_length: 65_536,
      input_modalities: ["text", "image"],
      output_modalities: ["text"],
      supported_parameters: ["tools", "temperature"],
      pricing: { prompt: "0.0000002", completion: "0.0000008" },
    },
    config,
  );

  assert.ok(model);
  assert.equal(model.thinkingFormat, "qwen");
  assert.equal(model.reasoning, true);
  assert.equal(model.toolCall, true);
  assert.deepEqual(model.input, ["text", "image"]);
  assert.deepEqual(model.cost, { input: 0.2, output: 0.8, cacheRead: 0, cacheWrite: 0 });
  assert.equal(model.contextWindow, 262_144);
  assert.equal(model.maxOutputTokens, 65_536);
});

test("filters non-TEE, disallowed, and embedding-only models", () => {
  assert.equal(mapAciModel({ id: "qwen/qwen3-coder", is_tee: false }, config), undefined);
  assert.equal(mapAciModel({ id: "other/model", is_tee: true }, config), undefined);
  assert.equal(
    mapAciModel(
      { id: "qwen/qwen3-coder", is_tee: true, output_modalities: ["embeddings"] },
      config,
    ),
    undefined,
  );
});

test("discovers the public model catalog without an authorization header", async () => {
  let request: Request | undefined;
  await discoverAciModelCatalog({
    config,
    fetch: async (input, init) => {
      request = new Request(input, init);
      return Response.json({ data: [] });
    },
  });

  assert.ok(request);
  assert.equal(request.headers.get("authorization"), null);
  assert.equal(new URL(request.url).pathname, "/v1/models");
});
