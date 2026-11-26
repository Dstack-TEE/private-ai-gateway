import { createHash } from "node:crypto";
import { readFile, rename, rm, writeFile } from "node:fs/promises";

const directory = new URL("../agent-bridge/resources/codex/", import.meta.url);
const destination = new URL("models.json", directory);
const temporary = new URL("models.json.tmp", directory);
const promptDestination = new URL("prompt.md", directory);
const promptTemporary = new URL("prompt.md.tmp", directory);

async function download(path, sha256, version) {
  const source = `https://raw.githubusercontent.com/openai/codex/rust-v${version}/codex-rs/models-manager/${path}`;
  const response = await fetch(source, { signal: AbortSignal.timeout(30_000) });
  if (!response.ok) throw new Error(`${path} download failed: HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (createHash("sha256").update(bytes).digest("hex") !== sha256) {
    throw new Error(`${path} SHA-256 differs from the reviewed pin`);
  }
  return bytes;
}

try {
  const pin = JSON.parse(await readFile(new URL("manifest.json", directory), "utf8"));
  if (!/^\d+\.\d+\.\d+$/.test(pin.version) || !/^[a-f0-9]{64}$/.test(pin.sha256)
      || !/^[a-f0-9]{64}$/.test(pin.promptSha256)
      || !Number.isSafeInteger(pin.modelCount) || pin.modelCount < 1) {
    throw new Error("Invalid Codex catalog pin");
  }
  const bytes = await download("models.json", pin.sha256, pin.version);
  // The instructions Codex gives a model it does not know; PAP gives them to
  // the provider's models.
  const prompt = await download("prompt.md", pin.promptSha256, pin.version);
  if (!prompt.toString("utf8").trim()) throw new Error("Codex prompt is empty");
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
  await writeFile(promptTemporary, prompt);
  await rename(temporary, destination);
  await rename(promptTemporary, promptDestination);
  console.log(`Refreshed Codex ${pin.version}: ${models.length} models (${pin.sha256})`);
} catch (error) {
  console.error(error instanceof Error ? error.message : "Catalog refresh failed");
  process.exitCode = 1;
} finally {
  await rm(temporary, { force: true });
  await rm(promptTemporary, { force: true });
}
