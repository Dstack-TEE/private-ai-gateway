import { expect, test } from "@playwright/test";
import { localApiExample } from "../src/renderer/lib/local-api-example";
import { localAddressKind } from "../src/renderer/components/listen-address";
import { createServer } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { nav, choose } from "./helpers";

test("CLI registration and explicit backend recovery are reachable", async ({ page }) => {
  await page.goto("/?mock=ready");
  await page.getByRole("navigation", { name: "Main navigation" }).getByRole("button", { name: "Settings" }).click();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  const cli = page.locator(".settings-advanced");
  await cli.getByRole("button", { name: "Install", exact: true }).click();
  await cli.getByRole("button", { name: "Remove", exact: true }).click();
  await expect(cli.getByRole("button", { name: "Install", exact: true })).toBeVisible();
  await page.goto("/?mock=backend-disconnected");
  await page.getByRole("button", { name: "Start backend", exact: true }).click();
  await expect(page.getByRole("button", { name: "Start backend", exact: true })).toHaveCount(0);
});

test("CLI startup errors remain visible until a successful retry", async ({ page }) => {
  await page.goto("/?mock=cli-startup-error");
  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  const cli = page.locator(".settings-advanced");
  await expect(cli).toContainText("Move Private AI Proxy to a stable location");
  await cli.getByRole("button", { name: "Install", exact: true }).click();
  await expect(cli).not.toContainText("Move Private AI Proxy to a stable location");
  await expect(cli.getByRole("button", { name: "Remove", exact: true })).toBeVisible();
});

test("Local API examples safely embed the local client credential", async () => {
  const calls: { url?: string; authorization?: string; body: unknown }[] = [];
  const server = createServer((request, response) => {
    const chunks: Buffer[] = [];
    request.on("data", (chunk: Buffer) => chunks.push(chunk));
    request.on("end", () => {
      try {
        calls.push({ url: request.url, authorization: request.headers.authorization, body: JSON.parse(Buffer.concat(chunks).toString()) });
        response.writeHead(200, { "Content-Type": "application/json" });
        response.end('{"ok":true}');
      } catch { response.writeHead(400); response.end(); }
    });
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const address = server.address();
    if (!address || typeof address === "string") throw new Error("Missing test endpoint");
    const endpoint = `http://127.0.0.1:${address.port}`;
    const model = "vendor/model'\nJSON\nquoted\"";
    const run = promisify(execFile);
    for (const language of ["curl", "python", "javascript"] as const) {
      const code = localApiExample(language, endpoint, model, "test-local-client-key");
      expect(code).toContain("test-local-client-key");
      if (language === "curl") await run("/bin/sh", ["-c", code], { timeout: 5_000 });
      else {
        expect(code).toContain("OpenAI");
        expect(code).toContain("client.chat.completions.create");
        if (language === "python") await run("python3", ["-c", "import ast,sys;ast.parse(sys.argv[1])", code]);
        else expect(code).toContain('import OpenAI from "openai"');
      }
    }
    expect(calls).toHaveLength(1);
    for (const call of calls) expect(call).toEqual({ url: "/v1/chat/completions", authorization: "Bearer test-local-client-key", body: { model, messages: [{ role: "user", content: "Hello" }] } });
  } finally {
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }
});

test("Local API help opens examples with model selection and copy actions", async ({ page }) => {
  await page.goto("/?mock=ready");
  await page.getByRole("button", { name: "Local API examples", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Local API examples", exact: true });
  await expect(dialog.locator("code")).toContainText("http://127.0.0.1:4180/v1");
  await expect(dialog.getByText("Available", { exact: true })).toHaveCount(0);
  await expect(dialog.locator("code")).toContainText("sk-pap-");
  await choose(page, dialog.getByRole("combobox", { name: "Model", exact: true }), "Z.ai: GLM 5.2");
  for (const language of ["cURL", "Python", "JavaScript"]) {
    await dialog.getByRole("tab", { name: language, exact: true }).click();
    await expect(dialog.locator("code")).toContainText("zai/glm-5.2");
    await expect(dialog.locator("code")).toContainText("sk-pap-");
    await dialog.getByRole("button", { name: "Copy example", exact: true }).click();
    await expect(dialog.getByRole("status")).toHaveText("Example copied");
  }
  await dialog.getByRole("button", { name: "Done", exact: true }).click();
  await expect(dialog).toHaveCount(0);
});

test("Notifications defaults on, preserves category choices and reports permission problems", async ({ page }) => {
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Notifications", exact: true }).click();
  const sheet = page.getByRole("dialog", { name: "Notifications" });
  const toggle = sheet.getByRole("switch", { name: "Allow notifications" });
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  const category = sheet.getByRole("switch", { name: "Gateway problems" });
  await expect(category).toHaveAttribute("aria-checked", "true");
  await category.click();
  await expect(category).toHaveAttribute("aria-checked", "false");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await expect(category).toBeDisabled();
  await page.reload();
  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Notifications", exact: true }).click();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await toggle.click();
  await expect(category).toBeEnabled();
  await expect(category).toHaveAttribute("aria-checked", "false");
  await page.goto("/?mock=notification-save-error&native-dialog=notifications");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await expect(toggle).toBeEnabled();
  await expect(sheet.getByRole("alert")).toContainText("Could not save notification settings.");
  await page.goto("/?mock=notifications-denied");
  await expect(page.getByRole("alert")).toHaveCount(0);
  await nav(page, "Settings").click();
  await expect(page.getByRole("alert")).toHaveCount(0);
  await page.getByRole("button", { name: "Notifications", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("Notifications are disabled in system settings.");
  await page.getByRole("button", { name: "System Settings", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-notification-settings-opened", "true");
  await page.goto("/?mock=notifications-prompt&native-dialog=notifications");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "false");
  await toggle.click();
  await expect(toggle).toHaveAttribute("aria-checked", "true");
  await expect(sheet.getByRole("alert")).toHaveCount(0);
});

test("notification authorization and banner settings remain distinct", async ({ page }) => {
  await page.goto("/?mock=notifications-no-banners&native-dialog=notifications");
  const sheet = page.getByRole("dialog", { name: "Notifications" });
  await expect(sheet.getByRole("alert")).toContainText("Notifications are allowed, but banner alerts are disabled");
  await expect(sheet.getByRole("button", { name: "Allow Notifications", exact: true })).toHaveCount(0);
  await sheet.getByRole("button", { name: "System Settings", exact: true }).click();
  await expect(page.locator("html")).toHaveAttribute("data-notification-settings-opened", "true");
  await page.goto("/?mock=wake-monitor-unavailable");
  await nav(page, "Settings").click();
  await expect(page.getByRole("alert")).toContainText("System wake monitoring is unavailable");
});

test("appearance defaults to system, persists and settings shortcut navigates", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/?mock=ready");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Settings", exact: true })).toBeVisible();
  const theme = page.getByRole("group", { name: "Theme", exact: true });
  await expect(theme.getByRole("button", { name: "System" })).toHaveAttribute("aria-pressed", "true");
  const order = await page.getByRole("region", { name: "General", exact: true }).locator('[data-slot="item-title"]').allTextContents();
  expect(order.indexOf("Theme")).toBe(order.indexOf("Protect on launch") + 1);
  await theme.getByRole("button", { name: "Light" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.keyboard.press("Control+,");
  await theme.getByRole("button", { name: "Dark" }).click();
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await theme.getByRole("button", { name: "System" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

test("diagnostics export has success and error feedback", async ({ page }) => {
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  await page.getByRole("button", { name: "Export diagnostics" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Diagnostics exported" })).toContainText("without keys");
  await page.goto("/?mock=export-error");
  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  await page.getByRole("button", { name: "Export diagnostics" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Could not export diagnostics." })).toBeVisible();
});

test("network listeners show discovered addresses and require explicit save consent", async ({ page }) => {
  for (const address of ["127.0.0.1", "127.3.2.1", "::1", "0:0:0:0:0:0:0:1"]) expect(localAddressKind(address)).toBe("loopback");
  expect(localAddressKind("0:0:0:0:0:0:0:0")).toBe("unspecified");
  expect(localAddressKind("not an address")).toBeUndefined();
  await page.setViewportSize({ width: 560, height: 680 });
  await page.goto("/?mock=ready&native-dialog=local-api");
  const sheet = page.getByRole("dialog", { name: "Local API settings" });
  const input = sheet.getByRole("combobox", { name: "Listen address" });
  await input.focus();
  await expect(input).toBeFocused();
  const gutter = await input.evaluate((element) => {
    const field = element.closest('[data-slot="input-group"]')?.getBoundingClientRect();
    const scroll = element.closest(".sheet-scroll")?.getBoundingClientRect();
    if (!field || !scroll) throw new Error("Missing scroll geometry");
    return Math.min(field.left - scroll.left, scroll.right - field.right, field.top - scroll.top);
  });
  expect(gutter).toBeGreaterThanOrEqual(3);
  await sheet.getByRole("button", { name: "Choose listen address" }).click();
  await page.getByRole("option", { name: "192.168.1.20 en0" }).click();
  await expect(input).toHaveValue("192.168.1.20");
  await sheet.getByRole("button", { name: "Network access warning" }).hover();
  await expect(page.getByRole("tooltip").and(page.locator("[data-open]"))).toContainText("unencrypted HTTP");
  page.once("dialog", (dialog) => { expect(dialog.message()).toContain("192.168.1.20"); void dialog.dismiss(); });
  await sheet.getByRole("button", { name: "Save", exact: true }).click();
  await expect(sheet).toBeVisible();
  await expect(sheet.getByRole("button", { name: "Save", exact: true })).toBeEnabled();
  page.once("dialog", (dialog) => dialog.accept());
  await sheet.getByRole("button", { name: "Save", exact: true }).click();
  await expect(sheet).toHaveCount(0);

  await page.goto("/?mock=network-scan-error&native-dialog=local-api");
  await expect(page.getByRole("status")).toContainText("Network interfaces unavailable");
  await page.getByRole("combobox", { name: "Listen address" }).fill("0:0:0:0:0:0:0:0");
  await expect(page.getByLabel("Client host", { exact: true })).toHaveAttribute("required", "");
});

test("model stacks render under production-style CSP without dynamic style tags", async ({ page }) => {
  await page.route((url) => url.pathname === "/", async (route) => {
    const response = await route.fetch();
    await route.fulfill({ response, headers: { ...response.headers(), "content-security-policy": "default-src 'self'; style-src 'self'; img-src 'self'; connect-src 'self'" } });
  });
  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();
  const chart = page.locator('[data-slot="chart"]');
  await expect(chart.locator("style")).toHaveCount(0);
  await expect.poll(() => chart.locator(".recharts-rectangle").evaluateAll((nodes) => nodes.filter((node) => { const box = node.getBoundingClientRect(); return box.width > 0 && box.height > 0; }).length)).toBeGreaterThan(0);
  expect(await chart.locator(".recharts-rectangle").evaluateAll((nodes) => new Set(nodes.map((node) => getComputedStyle(node).fill)).size)).toBeGreaterThan(1);
  const headers = await page.getByRole("table", { name: "Usage by model", includeHidden: true }).locator("thead th").allTextContents();
  expect(headers).toContain("openai/gpt-oss-20b");
  expect(headers).not.toContain("Input");
  expect(headers).not.toContain("Output");
  await page.getByRole("button", { name: /^Date range:/ }).click();
  const picker = page.getByRole("dialog", { name: "Choose date range" });
  await expect(picker.locator('[data-slot="calendar"]')).toBeVisible();
  await picker.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("table", { name: "Usage history", exact: true }).getByLabel("Token details", { exact: true }).first().hover();
  await expect(page.getByRole("tooltip").and(page.locator("[data-open]"))).toContainText("Cache read");
});
