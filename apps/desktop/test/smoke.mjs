import { mkdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import electronPath from "electron";
import { _electron as electron } from "playwright-core";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const outputDir = path.join(appRoot, "test-results");
await mkdir(outputDir, { recursive: true });

const electronApp = await electron.launch({
  executablePath: electronPath,
  args: ["--no-sandbox", appRoot],
  cwd: appRoot,
  env: {
    ...process.env,
    ACI_DESKTOP_CLI: path.join(appRoot, "test/mock-aci.mjs"),
  },
});

try {
  const page = await electronApp.firstWindow();
  await page.getByRole("button", { name: "Start" }).click();
  await page.getByText("Verified", { exact: true }).waitFor();
  await page.getByText("rcpt-desk...e-0001", { exact: true }).waitFor();
  await page.screenshot({ path: path.join(outputDir, "desktop.png"), fullPage: true });

  await electronApp.evaluate(({ BrowserWindow }) => {
    const window = BrowserWindow.getAllWindows()[0];
    window?.setSize(800, 640);
  });
  await page.waitForTimeout(150);
  await page.screenshot({ path: path.join(outputDir, "compact.png"), fullPage: true });

  const lifecycle = await electronApp.evaluate(({ BrowserWindow }) => {
    const window = BrowserWindow.getAllWindows()[0];
    window?.close();
    return {
      exists: window !== undefined,
      visible: window?.isVisible() ?? false,
    };
  });
  if (!lifecycle.exists || lifecycle.visible) {
    throw new Error(`Window did not hide to the system tray: ${JSON.stringify(lifecycle)}`);
  }
} finally {
  await electronApp.close();
}
