import { defineConfig } from "@playwright/test";

/**
 * Renderer smoke against the built bundle with the stateful `interactive`
 * mock: exercises the real UI flow (start/stop, key save/delete,
 * connect/disconnect, dialogs) without a desktop backend.
 */
export default defineConfig({
  testDir: "test",
  outputDir: "playwright-artifacts",
  timeout: 30_000,
  use: { baseURL: "http://127.0.0.1:4173" },
  webServer: {
    command: "npm run preview",
    url: "http://127.0.0.1:4173",
    reuseExistingServer: false,
  },
  reporter: [["list"]],
});
