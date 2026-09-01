import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { access, mkdir, readdir } from "node:fs/promises";
import path from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

import { _electron as electron } from "playwright-core";

if (process.platform !== "darwin") {
  throw new Error("The packaged macOS smoke test must run on macOS");
}

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const releaseRoot = path.join(appRoot, "release");
const outputDir = path.join(appRoot, "test-results");
const execFileAsync = promisify(execFile);
await mkdir(outputDir, { recursive: true });

const appBundle = await findAppBundle(releaseRoot);
const executablePath = path.join(
  appBundle,
  "Contents",
  "MacOS",
  "Private AI Gateway",
);
await access(executablePath);

const electronApp = await electron.launch({ executablePath });
try {
  const page = await electronApp.firstWindow();
  await page.getByRole("button", { name: "Start" }).click();
  await page.getByText("Verified", { exact: true }).waitFor({ timeout: 180_000 });

  const proxyUrl = await page.locator(".endpoint-row code").first().textContent();
  assert.ok(proxyUrl?.startsWith("http://127.0.0.1:"), `Unexpected proxy URL: ${proxyUrl}`);

  const response = await fetch(`${proxyUrl}/v1/models`, {
    signal: AbortSignal.timeout(30_000),
  });
  assert.equal(response.status, 200);
  const body = await response.json();
  assert.ok(isRecord(body) && Array.isArray(body.data) && body.data.length > 0);

  await page.screenshot({
    path: path.join(outputDir, "macos-packaged-verified.png"),
    fullPage: true,
  });
  await execFileAsync("screencapture", [
    "-x",
    path.join(outputDir, "macos-desktop-menubar.png"),
  ]);

  const lifecycle = await electronApp.evaluate(({ BrowserWindow }) => {
    const window = BrowserWindow.getAllWindows()[0];
    window?.close();
    return {
      exists: window !== undefined,
      visible: window?.isVisible() ?? false,
    };
  });
  assert.deepEqual(lifecycle, { exists: true, visible: false });
} finally {
  await electronApp.close();
}

async function findAppBundle(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  for (const entry of entries) {
    const entryPath = path.join(directory, entry.name);
    if (entry.isDirectory() && entry.name === "Private AI Gateway.app") {
      return entryPath;
    }
    if (entry.isDirectory()) {
      try {
        return await findAppBundle(entryPath);
      } catch (error) {
        if (!(error instanceof Error) || error.message !== "App bundle not found") {
          throw error;
        }
      }
    }
  }
  throw new Error("App bundle not found");
}

function isRecord(value) {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
