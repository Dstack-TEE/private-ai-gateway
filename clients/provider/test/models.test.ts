import assert from "node:assert/strict";
import test from "node:test";

import { resolveAciProviderConfig } from "../src/config.ts";
import { discoverAciModelCatalog, mapAciModel } from "../src/models.ts";
import { resolveAciProviderProfile } from "../src/profile.ts";

const profile = resolveAciProviderProfile({ defaultBaseURL: "https://gateway.example/v1" });
const config = resolveAciProviderConfig(profile, {
  models: { isTeeOnly: true, allowlist: ["provider/model"] },
});

const catalogModel = {
  id: "provider/model",
  name: "Provider Model",
  is_tee: true,
  context_length: 262_144,
  max_output_length: 65_536,
  input_modalities: ["text", "image"],
  output_modalities: ["text"],
  supported_features: ["reasoning", "tools"],
  supported_sampling_parameters: ["temperature"],
  pricing: { prompt: "0.0000002", completion: "0.0000008" },
};

test("maps an ACI catalog entry into framework-neutral provider capabilities", () => {
  const model = mapAciModel(catalogModel, config);

  assert.ok(model);
  assert.equal(model.reasoning, true);
  assert.equal(model.toolCall, true);
  assert.equal(model.temperature, true);
  assert.deepEqual(model.input, ["text", "image"]);
  assert.deepEqual(model.cost, { input: 0.2, output: 0.8 });
  assert.equal(model.contextWindow, 262_144);
  assert.equal(model.maxOutputTokens, 65_536);
});

test("filters non-TEE and disallowed models", () => {
  assert.equal(mapAciModel({ id: "provider/model", is_tee: false }, config), undefined);
  assert.equal(mapAciModel({ id: "other/model", is_tee: true }, config), undefined);
});

test("does not advertise capabilities that the catalog does not declare", () => {
  const model = mapAciModel(
    {
      ...catalogModel,
      supported_features: [],
      supported_sampling_parameters: [],
    },
    config,
  );

  assert.ok(model);
  assert.equal(model.reasoning, false);
  assert.equal(model.toolCall, false);
  assert.equal(model.temperature, false);
});

test("rejects malformed catalog facts instead of inventing defaults", () => {
  assert.throws(
    () => mapAciModel({ ...catalogModel, context_length: Number.POSITIVE_INFINITY }, config),
    /context_length must be a positive safe integer/,
  );
  assert.throws(
    () => mapAciModel({ ...catalogModel, output_modalities: ["embeddings"] }, config),
    /output_modalities must be an array of supported modalities/,
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
