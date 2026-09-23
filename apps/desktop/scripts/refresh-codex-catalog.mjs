import { createHash } from "node:crypto";
import { readFile, rename, rm, writeFile } from "node:fs/promises";

const directory = new URL("../agent-bridge/resources/codex/", import.meta.url);
const destination = new URL("models.json", directory);
const temporary = new URL("models.json.tmp", directory);

try {
  const pin = JSON.parse(await readFile(new URL("manifest.json", directory), "utf8"));
  if (!/^\d+\.\d+\.\d+$/.test(pin.version) || !/^[a-f0-9]{64}$/.test(pin.sha256)
      || !Number.isSafeInteger(pin.modelCount) || pin.modelCount < 1) {
    throw new Error("Invalid Codex catalog pin");
  }
  const source = `https://raw.githubusercontent.com/openai/codex/rust-v${pin.version}/codex-rs/models-manager/models.json`;
  const response = await fetch(source, { signal: AbortSignal.timeout(30_000) });
  if (!response.ok) throw new Error(`Catalog download failed: HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (createHash("sha256").update(bytes).digest("hex") !== pin.sha256) {
    throw new Error("Catalog SHA-256 differs from the reviewed pin");
  }
  const catalog = JSON.parse(bytes.toString("utf8"));
  const models = catalog.models;
  if (!Array.isArray(models) || models.length !== pin.modelCount
      || new Set(models.map((model) => model.slug)).size !== models.length
      || models.some((model) => typeof model.slug !== "string" || !model.slug
        || typeof model.model_messages?.instructions_template !== "string"
        || !model.model_messages.instructions_template.trim()
        || Object.hasOwn(model, "base_instructions"))) {
    throw new Error("Catalog does not match the supported bundled structure");
  }
  await writeFile(temporary, bytes);
  await rename(temporary, destination);
  console.log(`Refreshed Codex ${pin.version}: ${models.length} models (${pin.sha256})`);
} catch (error) {
  console.error(error instanceof Error ? error.message : "Catalog refresh failed");
  process.exitCode = 1;
} finally {
  await rm(temporary, { force: true });
}
