import assert from "node:assert/strict";
import { test } from "node:test";

import { DEFAULT_ACI_CLOUD_CONFIG } from "../src/config.ts";
import { inferThinkingFormat, mapAciServerModel } from "../src/models.ts";
import type { AciServerModel } from "../src/models.ts";

test("inferThinkingFormat: qwen models use qwen format with enable_thinking", () => {
  const result = inferThinkingFormat("phala/qwen3.5-27b");
  assert.equal(result.reasoning, true);
  assert.equal(result.format, "qwen");
  assert.equal(result.maxTokensField, "max_tokens");
});

test("inferThinkingFormat: gpt-oss models use openai reasoning_effort", () => {
  const result = inferThinkingFormat("phala/gpt-oss-20b");
  assert.equal(result.reasoning, true);
  assert.equal(result.format, "openai");
  assert.equal(result.maxTokensField, "max_completion_tokens");
  assert.equal(result.supportsReasoningEffort, true);
});

test("inferThinkingFormat: gemma and other non-reasoning models default to off", () => {
  const result = inferThinkingFormat("phala/gemma-3-27b-it");
  assert.equal(result.reasoning, false);
  assert.equal(result.format, "off");
});

test("inferThinkingFormat: deepseek-r1 treated as openai reasoning", () => {
  const result = inferThinkingFormat("deepseek/deepseek-r1");
  assert.equal(result.reasoning, true);
  assert.equal(result.format, "openai");
});

test("mapAciServerModel: drops non-TEE models when isTeeOnly is true", () => {
  const model: AciServerModel = {
    id: "some/plain-model",
    name: "Plain",
    is_tee: false,
    context_length: 32768,
    max_output_length: 8192,
    pricing: { prompt: "0.00000010", completion: "0.00000020" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.equal(mapped, null);
});

test("mapAciServerModel: keeps non-TEE models when isTeeOnly is false", () => {
  const config = { ...DEFAULT_ACI_CLOUD_CONFIG, models: { ...DEFAULT_ACI_CLOUD_CONFIG.models, isTeeOnly: false } };
  const model: AciServerModel = {
    id: "some/plain-model",
    name: "Plain",
    is_tee: false,
    context_length: 32768,
    max_output_length: 8192,
    pricing: { prompt: "0.00000010", completion: "0.00000020" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, config);
  assert.ok(mapped);
  assert.equal(mapped.id, "some/plain-model");
});

test("mapAciServerModel: excludes embedding models", () => {
  const model: AciServerModel = {
    id: "qwen/qwen3-embedding-8b",
    name: "Embedding",
    is_tee: true,
    context_length: 32000,
    output_modalities: ["embeddings"],
    input_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.equal(mapped, null);
});

test("mapAciServerModel: converts per-token pricing to per-million", () => {
  const model: AciServerModel = {
    id: "phala/qwen3.5-27b",
    name: "Qwen3.5 27B",
    is_tee: true,
    context_length: 262144,
    max_output_length: 262144,
    pricing: { prompt: "0.00000030", completion: "0.00000240" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.ok(mapped);
  assert.equal(mapped.cost.input, 0.3);
  assert.equal(mapped.cost.output, 2.4);
  assert.equal(mapped.cost.cacheRead, 0);
});

test("mapAciServerModel: maps image input modality", () => {
  const model: AciServerModel = {
    id: "phala/qwen3-vl-30b",
    name: "Qwen3 VL",
    is_tee: true,
    context_length: 128000,
    pricing: { prompt: "0.00000020", completion: "0.00000070" },
    input_modalities: ["text", "image"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.ok(mapped);
  assert.deepEqual(mapped.input, ["text", "image"]);
});

test("mapAciServerModel: allowlist filters out unlisted ids", () => {
  const config = {
    ...DEFAULT_ACI_CLOUD_CONFIG,
    models: { ...DEFAULT_ACI_CLOUD_CONFIG.models, allowlist: ["phala/qwen3.5-27b"] },
  };
  const kept: AciServerModel = {
    id: "phala/qwen3.5-27b",
    is_tee: true,
    context_length: 1000,
    pricing: { prompt: "0", completion: "0" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const dropped: AciServerModel = {
    id: "phala/other-model",
    is_tee: true,
    context_length: 1000,
    pricing: { prompt: "0", completion: "0" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  assert.ok(mapAciServerModel(kept, config));
  assert.equal(mapAciServerModel(dropped, config), null);
});

test("mapAciServerModel: qwen model gets qwen thinkingFormat compat", () => {
  const model: AciServerModel = {
    id: "phala/qwen3.5-27b",
    is_tee: true,
    context_length: 262144,
    pricing: { prompt: "0", completion: "0" },
    input_modalities: ["text"],
    output_modalities: ["text"],
  };
  const mapped = mapAciServerModel(model, DEFAULT_ACI_CLOUD_CONFIG);
  assert.ok(mapped);
  assert.equal(mapped.reasoning, true);
  assert.equal((mapped.compat as { thinkingFormat?: string } | undefined)?.thinkingFormat, "qwen");
  assert.equal((mapped.compat as { maxTokensField?: string } | undefined)?.maxTokensField, "max_tokens");
});
