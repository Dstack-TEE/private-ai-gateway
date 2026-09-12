import { expect, test } from "@playwright/test";
import { modelChartData } from "../src/renderer/components/usage-chart";
import { usageDateBounds } from "../src/renderer/lib/usage-dates";
import { localApiExample } from "../src/renderer/lib/local-api-example";
import { localAddressKind } from "../src/renderer/components/listen-address";
import { createServer } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

type Page = import("@playwright/test").Page;
async function choose(page: Page, control: import("@playwright/test").Locator, label: string) {
  await control.click();
  await page.getByRole("option", { name: label, exact: true }).click();
}

test("compact overview separates provider verification from current-session usage", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 1040 });
  await page.goto("/?mock=ready");
  const protection = page.getByRole("region", { name: "Protection status", exact: true });
  const agentCard = page.locator(".overview-module").filter({ has: page.getByRole("heading", { name: "Agents", exact: true }) });
  await expect(agentCard.locator(".agent-block")).toHaveCount(3);
  const bottomSpace = await agentCard.evaluate((node) => {
    const frame = node.querySelector(".module")?.getBoundingClientRect();
    const last = Array.from(node.querySelectorAll(".agent-block")).at(-1)?.getBoundingClientRect();
    if (!frame || !last) throw new Error("Missing agent card");
    return frame.bottom - last.bottom;
  });
  expect(bottomSpace).toBeLessThanOrEqual(1.5);
  expect(await page.locator(".content").evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
  const margins = await page.locator(".content").evaluate((node) => {
    const frame = node.getBoundingClientRect();
    const overview = node.querySelector(".overview-page");
    if (!overview) throw new Error("Missing overview");
    const content = overview.getBoundingClientRect();
    return [content.left - frame.left, frame.right - content.right, frame.bottom - content.bottom];
  });
  expect(margins).toEqual([24, 24, 24]);
  await expect(page.locator(".overview-page [data-slot=card]")).toHaveCount(5);
  await expect(agentCard.locator('[data-slot="separator"]')).toHaveCount(0);
  await expect(agentCard.locator('[data-slot="item"][data-variant="muted"]')).toHaveCount(3);
  await expect(page.locator('.session-summary > [data-slot="session-metric"]')).toHaveCount(3);
  await expect(page.locator('.session-summary > [data-slot="separator"]')).toHaveCount(2);
  const metricBottoms = await page.locator('[data-slot="session-metric"] strong').evaluateAll(nodes => nodes.map(node => node.getBoundingClientRect().bottom));
  expect(new Set(metricBottoms).size).toBe(1);
  const sessionBottomGap = await page.locator(".session-overview").evaluate((card) => {
    const content = card.querySelector(".session-summary");
    if (!content) throw new Error("Missing session summary");
    return card.getBoundingClientRect().bottom - content.getBoundingClientRect().bottom;
  });
  expect(sessionBottomGap).toBe(16);
  await expect(page.locator(".page-header")).toHaveCSS("border-bottom-width", "0px");
  await expect(page.locator(".session-summary")).toHaveCSS("gap", "12px");
  await expect(page.locator(".overview-top")).toHaveCSS("gap", "16px");
  await expect(page.locator(".overview-grid")).toHaveCSS("gap", "16px");
  await expect(page.locator("html")).toHaveAttribute("data-main-presented", "true");
  await expect(protection.getByText("Request protection", { exact: false })).toHaveCount(0);
  await expect(protection.getByText("Protect requests", { exact: true })).toHaveCount(0);
  await expect(protection.locator(".tracks-left, .status-glow, .status-local")).toHaveCount(0);
  const session = page.getByRole("region", { name: "Current session", exact: true });
  await expect(session.getByRole("heading", { name: "Current session" })).toBeVisible();
  await expect(agentCard.locator('[data-slot="card-description"]')).toHaveText("Use private AI in your agents.");
  await expect(page.getByText("Latest requests in this session.", { exact: true })).toBeVisible();
  await expect(session.locator('[data-slot="session-metric"]')).toHaveCount(3);
  await expect(session.getByText("Active", { exact: true })).toHaveCount(0);
  await protection.getByRole("button", { name: "Privacy verification" }).click();
  await expect(page.getByRole("dialog", { name: "Privacy verification" })).toBeVisible();
  await page.keyboard.press("Escape");
  await protection.getByRole("button", { name: "Profiles: RedPill" }).click();
  await expect(page.getByRole("dialog", { name: "Profiles" })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.goto("/?mock=interactive");
  await expect(session.getByText("Not active", { exact: true })).toHaveCount(0);
  await expect(session.locator("strong")).toHaveText(["—", "—", "—"]);
  for (const width of [540, 320]) {
    await page.setViewportSize({ width, height: 780 });
    expect(await page.locator(".content").evaluate((node) => node.scrollWidth - node.clientWidth)).toBe(0);
  }
});

test("development policy changes stop protection only after confirmation", async ({ page }) => {
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  const policy = page.getByRole("switch", { name: "Allow development OS" });
  await expect(policy).toBeEnabled();
  page.once("dialog", (dialog) => dialog.dismiss());
  await policy.click();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();
  await expect(policy).not.toBeChecked();
  page.once("dialog", (dialog) => dialog.accept());
  await policy.click();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();
  await expect(policy).toBeChecked();
});

test("startup requests notification permission and proof content keeps symmetric padding", async ({ page }) => {
  await page.goto("/?mock=notifications-prompt");
  await expect(page.locator("html")).toHaveAttribute("data-notification-permission-requested", "true");
  await expect(page.getByText(/System permission is needed/)).toHaveCount(0);
  await page.goto("/?mock=ready&native-dialog=usage-proof&record=51be02");
  const proof = page.locator(".proof-card");
  await expect(proof).toHaveCSS("padding-right", "0px");
  await expect(proof).toHaveCSS("scrollbar-gutter", "auto");
  expect(await proof.evaluate((node) => node.scrollWidth - node.clientWidth)).toBe(0);
});

test("deleting a live profile confirms stop and aborts if stopping fails", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=profiles");
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  const remove = editor.getByRole("button", { name: "Delete Profile" });
  await expect(remove).toBeEnabled();
  page.once("dialog", async (dialog) => { expect(dialog.message()).toContain("Protection will stop"); await dialog.dismiss(); });
  await remove.click();
  await expect(editor).toBeVisible();
  await expect(remove).toBeEnabled();
  page.once("dialog", (dialog) => dialog.accept());
  await remove.click();
  await expect(editor).toHaveCount(0);
  await expect(profiles.locator(".profile-select")).toHaveCount(0);

  await page.goto("/?mock=stop-protection-error&native-dialog=profiles");
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  page.once("dialog", (dialog) => dialog.accept());
  await remove.click();
  await expect(editor.getByRole("alert")).toHaveText("Could not stop protection");
  await expect(remove).toBeEnabled();
  await editor.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(profiles.getByRole("button", { name: "Edit RedPill" })).toBeVisible();
});

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

const nav = (page: Page, name: string) =>
  page.getByRole("navigation", { name: "Main navigation" }).getByRole("button", { name });

test("desktop suppresses browser reload menus and preserves native editing actions", async ({ page }) => {
  await page.addInitScript(() => window.addEventListener("mock:edit-menu", (event) => {
    if (event instanceof CustomEvent) document.documentElement.dataset.editMenu = JSON.stringify(event.detail);
  }));
  for (const path of ["/?mock=ready", "/?mock=ready&native-dialog=local-api"]) {
    await page.goto(path);
    await expect(page.getByRole("heading").first()).toBeVisible();
    const prevented = await page.evaluate(() => {
      const context = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
      document.body.dispatchEvent(context);
      const reloadKeys = [
        { key: "F5" }, { key: "r", metaKey: true },
        { key: "r", ctrlKey: true }, { key: "R", metaKey: true, shiftKey: true },
      ].map((init) => {
        const event = new KeyboardEvent("keydown", { ...init, bubbles: true, cancelable: true });
        window.dispatchEvent(event);
        return event.defaultPrevented;
      });
      const copy = new KeyboardEvent("keydown", { key: "c", ctrlKey: true, bubbles: true, cancelable: true });
      window.dispatchEvent(copy);
      const drop = new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: new DataTransfer() });
      drop.dataTransfer?.items.add(new File(["test"], "test.html", { type: "text/html" }));
      window.dispatchEvent(drop);
      return { context: context.defaultPrevented, reloadKeys, copy: copy.defaultPrevented, drop: drop.defaultPrevented };
    });
    expect(prevented).toEqual({ context: true, reloadKeys: [true, true, true, true], copy: false, drop: true });
    await expect(page.locator("html")).not.toHaveAttribute("data-edit-menu");
  }
  await page.getByLabel("Listen address", { exact: true }).click({ button: "right" });
  await expect(page.locator("html")).toHaveAttribute("data-edit-menu", '{"editable":true}');
  await page.getByLabel("Client key", { exact: true }).click({ button: "right" });
  await expect(page.locator("html")).toHaveAttribute("data-edit-menu", '{"editable":false}');
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
  await expect(page.locator("html")).toHaveCSS("color-scheme", "light");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.keyboard.press("Control+,");
  await theme.getByRole("button", { name: "Dark" }).click();
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await theme.getByRole("button", { name: "System" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

test("calendar month and year menus use Select without changing range semantics", async ({ page }) => {
  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();
  await page.getByRole("button", { name: "Date range: Last 7 days", exact: true }).click();
  const year = page.getByRole("combobox", { name: "Choose the Year", exact: true }).first();
  await choose(page, year, "2024");
  const month = page.getByRole("combobox", { name: "Choose the Month", exact: true }).first();
  await choose(page, month, "Jan");
  await expect(page.getByRole("button", { name: /Monday, January 1st, 2024/ })).toBeVisible();
  await expect(page.locator("select:visible")).toHaveCount(0);
  await month.click();
  await expect(page.locator('[data-slot="select-content"][data-open]')).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(month).toBeFocused();
  await expect(page.getByRole("button", { name: "Cancel", exact: true })).toBeVisible();
});

test("form focus rings have space on all four sides of their scroll viewport", async ({ page }) => {
  await page.setViewportSize({ width: 560, height: 680 });
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await page.goto("/?mock=ready&native-dialog=local-api");
    for (const id of ["local-listen-address", "local-port", "local-client-host", "local-client-key"]) {
      const control = page.locator(`#${id}`);
      await control.focus();
      const clearances = await control.evaluate((element) => {
        const ring = (element.closest('[data-slot="input-group"]') ?? element).getBoundingClientRect();
        const scroll = element.closest(".sheet-scroll")?.getBoundingClientRect();
        if (!scroll) throw new Error("Missing scroll viewport");
        return [ring.left - scroll.left, scroll.right - ring.right, ring.top - scroll.top, scroll.bottom - ring.bottom];
      });
      expect(Math.min(...clearances), `${id} in ${colorScheme}`).toBeGreaterThanOrEqual(3);
    }
  }
});

test("system dark styling is present before React initializes", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.route("**/*.js", (route) => route.request().url().endsWith("/appearance-init.js") ? route.continue() : route.abort());
  await page.goto("/?mock=ready&native-dialog=profiles");
  await expect(page.locator("html")).toHaveCSS("color-scheme", "dark");
  await expect(page.locator("body")).toHaveCSS("background-color", "oklch(0.145 0 0)");
});

test("visible Agents detects a newly installed OpenCode without reopening the page", async ({ page }) => {
  await page.clock.install();
  await page.goto("/?mock=agent-installed");
  await nav(page, "Agents").click();
  await expect(page.getByRole("region", { name: "Not installed", exact: true })).toContainText("OpenCode");
  await page.evaluate(() => { document.documentElement.dataset.mockAgentInstalled = "true"; });
  await page.clock.fastForward(15_000);
  await expect(page.getByRole("switch", { name: "Connect OpenCode", exact: true })).toBeVisible();
});

test("recent usage keeps ten rows in an internally scrollable card", async ({ page }) => {
  await page.setViewportSize({ width: 1100, height: 1040 });
  await page.goto("/?mock=recent-usage");
  const list = page.getByRole("region", { name: "Recent requests", exact: true });
  await expect(list.locator(".usage-row")).toHaveCount(10);
  expect(await list.evaluate((node) => node.scrollHeight > node.clientHeight)).toBe(true);
  await list.evaluate((node) => { node.scrollTop = node.scrollHeight; });
  await expect(list.locator(".usage-row").last()).toBeInViewport();
  await expect(page.getByRole("heading", { name: "Recent usage", exact: true })).toBeInViewport();
});

test("window activation redetects an uninstalled connected agent", async ({ page }) => {
  await page.goto("/?mock=agent-uninstalled");
  await nav(page, "Agents").click();
  await expect(page.getByRole("switch", { name: "Disconnect Claude Code", exact: true })).toBeVisible();
  await page.evaluate(() => {
    document.documentElement.dataset.mockAgentRemoved = "true";
    window.dispatchEvent(new Event("focus"));
  });
  await expect(page.getByRole("switch", { name: "Disconnect Claude Code", exact: true })).toHaveCount(0);
  await expect(page.getByRole("region", { name: "Not installed", exact: true })).toContainText("Claude Code");
  await nav(page, "Overview").click();
  await expect(page.getByRole("group", { name: "Installed agents" }).getByRole("img", { name: "Claude Code" })).toHaveCount(0);
});

const themeColor = (page: Page, token: string) => page.evaluate((name) => {
  const probe = document.createElement("span");
  probe.hidden = true;
  probe.style.color = `var(${name})`;
  document.body.append(probe);
  const color = getComputedStyle(probe).color;
  probe.remove();
  return color;
}, token);

const overflow = (page: Page) =>
  page.evaluate(() => {
    const content = document.querySelector<HTMLElement>(".content");
    return Math.max(
      document.documentElement.scrollWidth - document.documentElement.clientWidth,
      content ? content.scrollWidth - content.clientWidth : 0,
    );
  });

test("model chart aggregation preserves totals and uses monthly buckets for long ranges", () => {
  const day = new Date();
  day.setDate(day.getDate() - 120);
  const first = day.toISOString().slice(0, 10);
  const series = [{ day: first, requests: 2, inputTokens: 200, outputTokens: 100, tokens: 300, costUsd: 3 }];
  const modelSeries = [{ day: first, model: "model-a", requests: 1, tokens: 100, costUsd: 1 }, { day: first, model: "model-b", requests: 1, tokens: 200, costUsd: 2 }];
  for (const [metric, total] of [["tokens", 300], ["cost", 3], ["requests", 2]] as const) {
    const result = modelChartData({ series, modelSeries }, "all", metric);
    expect(result.monthly).toBe(true);
    expect(result.series.map((entry) => entry.label).sort()).toEqual(["model-a", "model-b"]);
    const sum = result.rows.reduce((sum, row) => sum + result.series.reduce((sum, entry) => sum + Number(row[entry.key] ?? 0), 0), 0);
    expect(sum).toBe(total);
    expect(result.rows.at(-1)?.period).toBe(new Date().toISOString().slice(0, 7));
  }
});

test("model colors survive filtering and Other preserves all three metrics", () => {
  const day = "2026-09-02";
  const modelSeries = Array.from({ length: 12 }, (_, index) => ({ day, model: `model-${index}`, requests: 1, tokens: index + 1, costUsd: index + 1 }));
  const models = modelSeries.map((point) => point.model);
  const series = [{ day, requests: 12, inputTokens: 78, outputTokens: 0, tokens: 78, costUsd: 78 }];
  const bounds = usageDateBounds({ preset: "custom", from: new Date(2026, 8, 2), to: new Date(2026, 8, 4) });
  expect(bounds.since).toBe(new Date(2026, 8, 2).getTime() / 1000);
  expect(bounds.until).toBe(new Date(2026, 8, 5).getTime() / 1000);
  for (const metric of ["requests", "tokens", "cost"] as const) {
    const chart = modelChartData({ models, series, modelSeries }, "custom", metric, bounds);
    expect(chart.rows.map((row) => row.period)).toEqual(["2026-09-02", "2026-09-03", "2026-09-04"]);
    expect(chart.series).toHaveLength(11);
    expect(chart.series.at(-1)?.label).toBe("Other");
    expect(chart.rows.reduce((sum, row) => sum + chart.series.reduce((total, entry) => total + Number(row[entry.key]), 0), 0)).toBe(metric === "requests" ? 12 : 78);
    const filtered = modelChartData({ models, series, modelSeries: modelSeries.filter((point) => point.model === "model-11") }, "custom", metric, bounds);
    expect(filtered.series[0]?.color).toBe(chart.series.find((entry) => entry.label === "model-11")?.color);
  }
});

test("custom date ranges apply atomically to chart and table", async ({ page }) => {
  await page.clock.setFixedTime(new Date("2026-09-06T12:00:00"));
  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();
  const dateButton = page.getByRole("button", { name: /^Date range:/ });
  await dateButton.click();
  await page.locator('[data-day="9/2/2026"]').click();
  await page.getByRole("dialog", { name: "Choose date range" }).getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dateButton).toHaveAccessibleName("Date range: Last 7 days");
  await dateButton.click();
  await choose(page, page.getByRole("combobox", { name: "Quick date range" }), "All time");
  await dateButton.click();
  await page.locator('[data-day="9/2/2026"]').click();
  await page.locator('[data-day="9/4/2026"]').click();
  await page.getByRole("dialog", { name: "Choose date range" }).getByRole("button", { name: "Apply", exact: true }).click();
  await expect(dateButton).toHaveAccessibleName("Date range: Sep 2, 2026 - Sep 4, 2026");
  const table = page.getByRole("table", { name: "Usage history", exact: true });
  await expect(page.getByRole("table", { name: "Usage by model", includeHidden: true }).locator("tbody th")).toHaveText(["2026-09-02", "2026-09-03", "2026-09-04"]);
  const timestamps = await table.locator("time").evaluateAll((nodes) => nodes.map((node) => Date.parse(node.getAttribute("datetime") ?? "")));
  expect(timestamps.length).toBeGreaterThan(0);
  expect(timestamps.every((time) => time >= new Date(2026, 8, 2).getTime() && time < new Date(2026, 8, 5).getTime())).toBe(true);

});

test("public preview frames the Tauri renderer as a macOS window and exposes the tray contract", async ({ page }) => {
  await page.setViewportSize({ width: 1100, height: 900 });
  await page.goto("/?mock=ready");

  const frame = page.locator(".desktop-window");
  const frameBox = await frame.boundingBox();
  expect(frameBox).not.toBeNull();
  expect(frameBox!.x).toBeGreaterThan(0);
  expect(frameBox!.y).toBeGreaterThanOrEqual(40);
  expect(frameBox!.width).toBeLessThan(1100);
  await expect(page.locator(".traffic-lights > span")).toHaveCount(3);

  await page.getByRole("button", { name: "Private AI Proxy menu" }).click();
  const tray = page.getByRole("menu", { name: "Private AI Proxy" });
  await expect(tray.getByRole("switch")).toHaveCount(0);
  await expect(tray.getByRole("menuitem", { name: "Stop protection" })).toBeVisible();
  for (const name of ["Open Private AI Proxy", "Settings…", "Quit Private AI Proxy"]) {
    await expect(tray.getByRole("menuitem", { name })).toBeVisible();
  }
  const openAtLogin = tray.getByRole("menuitemcheckbox", { name: "Open at Login" });
  await expect(openAtLogin).toHaveAttribute("aria-checked", "false");
  await openAtLogin.click();
  await expect(openAtLogin).toHaveAttribute("aria-checked", "true");

  const brandImageElements = page.locator(".brand-logo img");
  await expect(brandImageElements).toHaveCount(2);
  await expect.poll(() => brandImageElements.evaluateAll((images) =>
    images.every((image) => image instanceof HTMLImageElement && image.complete && image.naturalWidth > 0),
  )).toBe(true);
  const brandIcons = await brandImageElements.evaluateAll((images) =>
    images.map((image) => ({ source: (image as HTMLImageElement).currentSrc })),
  );
  expect(brandIcons).toHaveLength(2);
  expect(brandIcons.every(({ source }) => /brand-mark-(light|dark)/.test(source) && !source.startsWith("data:"))).toBe(true);
  for (const source of new Set(brandIcons.map((icon) => icon.source))) {
    const vector = await page.evaluate(async (url) => {
      const response = await fetch(url);
      const document = new DOMParser().parseFromString(await response.text(), "image/svg+xml");
      return {
        rasterEffects: document.querySelectorAll("filter, mask, image").length,
        vectorCutout: document.querySelector('clipPath path[clip-rule="evenodd"]') !== null,
      };
    }, source);
    expect(vector).toEqual({ rasterEffects: 0, vectorCutout: true });
  }
  await expect(page.locator(".tray-template-icon")).toHaveClass(/is-protected/);
  await expect(page.locator(".tray-template-icon")).toHaveCSS("mask-image", /tray-mark/);
  await expect(page.locator(".tray-template-icon")).toHaveCSS("opacity", "1");

  await tray.getByRole("menuitem", { name: "Settings…" }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();

  await page.getByRole("button", { name: "Profiles", exact: true }).click();
  const settingsDialog = page.getByRole("dialog", { name: "Profiles" });
  const dialogBox = await settingsDialog.boundingBox();
  expect(dialogBox).not.toBeNull();
  const frameCenter = frameBox!.x + frameBox!.width / 2;
  const dialogCenter = dialogBox!.x + dialogBox!.width / 2;
  expect(Math.abs(frameCenter - dialogCenter)).toBeLessThanOrEqual(2);
  await expect(settingsDialog.getByRole("heading", { name: "Profiles", exact: true })).toBeFocused();
  await expect(settingsDialog).toHaveCSS("outline-style", "none");

});

test("Usage chart preserves its layout while the initial query is pending", async ({ page }) => {
  await page.goto("/?mock=usage-query-pending");
  await nav(page, "Usage").click();
  const chart = page.locator(".usage-chart");
  await expect(chart).toHaveAttribute("aria-busy", "true");
  await expect(page.locator('.usage-over-time [data-slot="card-action"]').getByRole("tab", { name: "Tokens", exact: true })).toBeVisible();
  await expect(page.locator('.usage-stats > [data-slot="card"]')).toHaveCount(4);
  await expect(page.locator(".usage-over-time").getByText("Last 7 days", { exact: true })).toHaveCount(0);
  const before = await chart.boundingBox();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-usage-query")));
  await expect(chart).toHaveAttribute("aria-busy", "false");
  await expect(chart.locator(".recharts-surface")).toBeVisible();
  expect(await chart.boundingBox()).toEqual(before);
});

test("profile imports require confirmation, need credentials and preserve the active profile", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=profiles");
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  page.once("dialog", (dialog) => dialog.dismiss());
  await profiles.getByRole("button", { name: "Import profile configurations" }).click();
  await expect(profiles.getByRole("button", { name: "Edit Imported Phala" })).toHaveCount(0);
  page.once("dialog", (dialog) => dialog.accept());
  await profiles.getByRole("button", { name: "Import profile configurations" }).click();
  await expect(profiles.getByRole("status")).toHaveText("1 imported, 0 duplicates skipped.");
  const imported = profiles.locator(".profile-list-row", { hasText: "Imported Phala" });
  await expect(imported).toContainText("Sign in or add an API key");
  await expect(profiles.locator(".profile-select", { hasText: "RedPill" })).toHaveAttribute("aria-pressed", "true");
  page.once("dialog", (dialog) => dialog.accept());
  await profiles.getByRole("button", { name: "Import profile configurations" }).click();
  await expect(profiles.getByRole("status")).toHaveText("0 imported, 1 duplicates skipped.");
  await profiles.getByRole("button", { name: "Export profile configurations" }).click();
  await expect(profiles.getByRole("status")).toContainText("without credentials");
  await page.goto("/?mock=export-error&native-dialog=profiles");
  await profiles.getByRole("button", { name: "Export profile configurations" }).click();
  await expect(profiles.getByRole("alert")).toContainText("Could not export profile configurations.");
  await expect(profiles.getByRole("button", { name: "Done", exact: true })).toBeEnabled();
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

test("reset is in Advanced, requires consent and preserves profiles", async ({ page }) => {
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();
  await expect(page.getByText("Restore all agent configs", { exact: true })).toHaveCount(0);
  const reset = page.getByRole("button", { name: "Reset settings", exact: true });
  await expect(reset).not.toBeVisible();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  page.once("dialog", (dialog) => dialog.dismiss());
  await reset.click();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeChecked();
  page.once("dialog", (dialog) => {
    expect(dialog.message()).toContain("Profiles, credentials, the local API key, and usage history are kept");
    return dialog.accept();
  });
  await reset.click();
  await expect(page.getByRole("heading", { name: "Settings", exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).not.toBeChecked();
  await expect(page.getByRole("button", { name: "Profiles", exact: true })).toContainText("RedPill");
  await expect(page.getByRole("switch", { name: "Protect on launch" })).not.toBeChecked();
});

test("native event subscriptions isolate child dismissal from the Profiles window", async ({ page }) => {
  await page.addInitScript(() => {
    const state = {
      status: "stopped", configurationVerification: false, apiKeySaved: true,
      config: { remoteUrl: "https://tee.redpill.ai", requireProductionOs: true },
      localApi: { listenAddress: "127.0.0.1", port: 4180, allowNetworkAccess: false },
      activity: [], checks: [], sessionUsage: {}, activeProfileId: "work",
      profiles: [{ id: "work", name: "Work profile", provider: "redpill", remoteUrl: "https://tee.redpill.ai", credentialSaved: true }],
    };
    const callbacks = new Map<number, (event: unknown) => void>();
    const listeners = new Map<number, { event: string; target: { kind: string; label?: string }; handler: number }>();
    let serial = 0;
    Object.defineProperty(window, "__GATEWAY_INITIAL_STATE__", { configurable: true, value: state });
    Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {
      metadata: { currentWebview: { label: "profiles" }, currentWindow: { label: "profiles" } },
      transformCallback: (callback: (event: unknown) => void) => { const id = ++serial; callbacks.set(id, callback); return id; },
      unregisterCallback: (id: number) => callbacks.delete(id),
      invoke: async (command: string, args: { event?: string; target?: { kind: string; label?: string }; handler?: number; eventId?: number }) => {
        if (command === "plugin:event|listen" && args.event && args.handler !== undefined) {
          const id = ++serial;
          listeners.set(id, { event: args.event, target: args.target ?? { kind: "Any" }, handler: args.handler });
          if (args.event === "gateway://dialog-dismissed") document.documentElement.dataset.dismissListenerReady = "true";
          return id;
        }
        if (command === "plugin:event|unlisten" && args.eventId !== undefined) { listeners.delete(args.eventId); return; }
        if (command === "get_gateway_state") return state;
        if (command === "get_appearance") return "system";
        if (command === "get_notification_settings") return { preferences: { enabled: false }, permission: "denied" };
        if (command === "native_dialog_ready") document.documentElement.dataset.presented = "true";
      },
    } });
    window.addEventListener("test:child-dismiss", () => {
      for (const [id, listener] of listeners) {
        if (listener.event === "gateway://dialog-dismissed" && (listener.target.kind === "Any" || listener.target.label === "profile-editor")) {
          callbacks.get(listener.handler)?.({ event: listener.event, id, payload: null });
        }
      }
    });
  });
  await page.goto("/?native-dialog=profiles");
  await expect(page.locator("html")).toHaveAttribute("data-dismiss-listener-ready", "true");
  await expect(page.getByText("Work profile", { exact: true })).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("test:child-dismiss")));
  await expect(page.getByRole("dialog", { name: "Profiles", exact: true })).toBeVisible();
  await expect(page.getByText("Work profile", { exact: true })).toBeVisible();
});

test("Profiles keeps its list underneath the profile editor", async ({ page }) => {
  await page.setViewportSize({ width: 620, height: 560 });
  await page.goto("/?mock=no-key&native-dialog=profiles");

  await expect(page.locator(".desktop-window, .sidebar")).toHaveCount(0);
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  await expect(profiles).toBeVisible();
  const box = await profiles.boundingBox();
  expect(box).toEqual({ x: 0, y: 0, width: 620, height: 560 });

  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  await expect(page.locator(".profiles-sheet")).toHaveCount(1);
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor).toBeVisible();
  await expect(page.locator("dialog[open]")).toHaveCount(2);
  await expect(editor.getByRole("heading").first()).toBeFocused();
  await page.keyboard.press("Meta+w");
  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Profiles" })).toBeVisible();
  await expect(profiles.getByRole("button", { name: "Edit RedPill" })).toBeFocused();
});

test("native close requests and keyboard close respect a pending save", async ({ page }) => {
  await page.goto("/?mock=local-save-pending&native-dialog=local-api");
  const sheet = page.getByRole("dialog", { name: "Local API settings" });
  await expect(sheet.getByRole("heading").first()).toBeFocused();
  await sheet.getByLabel("Port", { exact: true }).fill("4181");
  await sheet.getByRole("button", { name: "Save", exact: true }).click();
  await expect(sheet.getByRole("button", { name: "Saving…", exact: true })).toBeDisabled();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:native-close")));
  await page.keyboard.press("Meta+w");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Meta+.");
  await expect(sheet).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-local-save")));
  await expect(sheet).toHaveCount(0);

  await page.goto("/?mock=ready&native-dialog=privacy");
  await expect(page.getByRole("heading", { name: "Privacy verification" })).toBeFocused();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:native-close")));
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("fixed sidebar does not consume Cmd+B or Ctrl+B", async ({ page }) => {
  await page.goto("/?mock=ready");
  await expect(nav(page, "Overview")).toBeVisible();
  const prevented = await page.evaluate(() => ["metaKey", "ctrlKey"].map((modifier) => {
    const event = new KeyboardEvent("keydown", { key: "b", [modifier]: true, cancelable: true, bubbles: true });
    window.dispatchEvent(event);
    return event.defaultPrevented;
  }));
  expect(prevented).toEqual([false, false]);
});

test("usage tooltips stay mounted while backend state and row data refresh", async ({ page }) => {
  await page.goto("/?mock=usage-live-refresh");
  await nav(page, "Usage").click();
  const trigger = page.getByRole("table", { name: "Usage history", exact: true }).getByLabel("Token details", { exact: true }).first();
  await trigger.hover();
  const tooltip = page.getByRole("tooltip").and(page.locator("[data-open]"));
  await expect(tooltip).toContainText("Cache read");
  const initialInput = Number((await tooltip.locator("dd").first().innerText()).replaceAll(",", ""));
  await tooltip.evaluate((node) => node.setAttribute("data-test-retained", "true"));
  await trigger.evaluate((node) => node.setAttribute("data-test-retained", "true"));
  for (let index = 1; index <= 3; index++) {
    await page.evaluate(() => window.dispatchEvent(new Event("mock:refresh-usage")));
    await expect(tooltip.locator("dd").first()).toHaveText((initialInput + index).toLocaleString("en-US"));
    await expect(tooltip).toHaveAttribute("data-test-retained", "true");
    await expect(trigger).toHaveAttribute("data-test-retained", "true");
  }
  await page.keyboard.press("Escape");
  await page.getByRole("table", { name: "Usage history", exact: true }).getByRole("button", { name: /View proof/ }).first().click();
  await expect(page.getByRole("dialog", { name: "Usage proof", exact: true })).toBeVisible();
});

test("Agents reserves three rows and elapsed time only appears while protected", async ({ page }) => {
  let fullHeight: number | undefined;
  for (const [scenario, count] of [["ready", 3], ["one-agent", 1], ["no-agents", 0]] as const) {
    await page.goto(`/?mock=${scenario}`);
    const card = page.locator(".overview-module").filter({ has: page.getByRole("heading", { name: "Agents", exact: true }) });
    await expect(card.locator(".agent-block")).toHaveCount(count);
    const height = await card.locator(".module").evaluate((node) => node.getBoundingClientRect().height);
    fullHeight ??= height;
    expect(height).toBe(fullHeight);
  }
  for (const scenario of ["interactive", "verifying", "blocked", "error", "reconnecting"]) {
    await page.goto(`/?mock=${scenario}`);
    await expect(page.getByLabel("Protection status").locator(".protection-duration")).toHaveCount(0);
    for (const card of await page.locator(".overview-top > [data-slot=card]").all()) {
      await expect(card).toHaveCSS("height", "144px");
    }
  }
  await page.goto("/?mock=ready");
  const duration = page.getByLabel("Protection status").locator(".protection-duration");
  await expect(duration).toBeVisible();
  for (const card of await page.locator(".overview-top > [data-slot=card]").all()) {
    await expect(card).toHaveCSS("height", "144px");
  }
  await page.clock.install();
  const initial = await duration.getAttribute("datetime");
  if (!initial) throw new Error("Missing session duration");
  const visibility = (hidden: boolean) => page.evaluate((value) => {
    Object.defineProperty(document, "hidden", { configurable: true, value });
    document.dispatchEvent(new Event("visibilitychange"));
  }, hidden);
  await visibility(true);
  await page.clock.fastForward(300_000);
  await expect(duration).toHaveAttribute("datetime", initial);
  await visibility(false);
  await expect(duration).not.toHaveAttribute("datetime", initial);
  const resumed = await duration.getAttribute("datetime");
  if (!resumed) throw new Error("Missing resumed session duration");
  expect(Number(resumed.slice(2, -1)) - Number(initial.slice(2, -1))).toBeGreaterThanOrEqual(300);
});

test("reconnection preserves visible session totals and can be cancelled", async ({ page }) => {
  await page.goto("/?mock=reconnecting");
  const card = page.getByLabel("Protection status");
  await expect(card.getByText("Reconnecting", { exact: true })).toBeVisible();
  await expect(card.locator(".protection-duration")).toHaveCount(0);
  await expect(card.getByRole("switch", { name: "Cancel reconnection" })).toBeChecked();
  await expect(page.locator(".session-summary strong").first()).not.toHaveText("—");
  await card.getByRole("switch", { name: "Cancel reconnection" }).click();
  await expect(card.getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.locator(".session-summary strong")).toHaveText(["—", "—", "—"]);
});

test("agent attention badges expose the correct recovery action", async ({ page }) => {
  await page.goto("/?mock=needs-attention");
  await nav(page, "Agents").click();
  await page.getByRole("button", { name: "Claude Code: Reconnect required" }).click();
  await expect(page.getByText("Gateway authentication settings changed.", { exact: false })).toBeVisible();
  await page.getByRole("button", { name: "Reconnect Claude Code", exact: true }).click();
  await expect(page.getByRole("button", { name: "Claude Code: Reconnect required" })).toHaveCount(0);
  await page.getByRole("button", { name: "OpenCode: Finish disconnecting" }).click();
  await page.getByRole("button", { name: "Disconnect OpenCode", exact: true }).click();
  await expect(page.getByRole("button", { name: "OpenCode: Finish disconnecting" })).toHaveCount(0);
});

test("local rejections explain why token usage is not applicable", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=usage-proof&record=local01");
  const missing = page.getByText("Not applicable", { exact: true });
  await expect(missing).toHaveCount(2);
  await missing.first().focus();
  await expect(page.getByRole("tooltip").and(page.locator("[data-open]"))).toContainText("blocked locally before forwarding");
});

test("help uses hover and focus tooltips in the main window and native dialogs", async ({ page }) => {
  await page.goto("/?mock=ready");
  const toggle = page.getByLabel("Protection status").getByRole("switch");
  await toggle.hover();
  await expect(toggle).not.toHaveAttribute("data-base-ui-tooltip-trigger");
  await expect(toggle).not.toHaveAttribute("title");
  await expect(page.getByRole("tooltip").and(page.locator("[data-open]"))).toHaveCount(0);
  const privacy = page.getByRole("button", { name: "Privacy verification", exact: true });
  await privacy.hover();
  await expect(page.getByRole("tooltip").and(page.locator("[data-open]"))).toHaveText("Privacy verification");
  await expect(privacy).not.toHaveAttribute("title");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await privacy.click();
  await expect(page.getByRole("dialog", { name: "Privacy verification" })).toBeVisible();
  await page.goto("/?mock=ready&native-dialog=local-api");
  const reveal = page.getByRole("button", { name: "Reveal client key", exact: true });
  await reveal.focus();
  const tooltip = page.getByRole("tooltip").and(page.locator("[data-open]"));
  await expect(tooltip).toHaveText("Reveal client key");
  expect(await tooltip.evaluate((node) => Boolean(node.closest("dialog")))).toBe(true);
  await reveal.click();
  await expect(page.getByLabel("Client key", { exact: true })).toHaveAttribute("type", "text");
});

test("overview profile and setup controls share compact dimensions", async ({ page }) => {
  for (const scenario of ["ready", "no-profiles"]) {
    await page.goto(`/?mock=${scenario}`);
    const card = page.getByLabel("Protection status");
    const profile = card.locator("#overview-profile");
    await expect(profile).toHaveCSS("width", "140px");
    await expect(profile).toHaveCSS("height", "32px");
    const bounds = await profile.boundingBox();
    const toggle = await card.getByRole("switch").boundingBox();
    const label = await card.locator('.status-heading [aria-live="polite"]').boundingBox();
    expect(bounds).not.toBeNull();
    expect(toggle).not.toBeNull();
    expect(label).not.toBeNull();
    expect(Math.abs((toggle?.y ?? 0) + (toggle?.height ?? 0) / 2 - (label?.y ?? 0) - (label?.height ?? 0) / 2)).toBeLessThanOrEqual(0.5);
    expect(toggle?.y).toBeLessThan(bounds?.y ?? 0);
    expect(toggle?.x).toBeGreaterThan((bounds?.x ?? 0) + (bounds?.width ?? 0));
    await expect(card.locator(".status-background-mark")).toHaveCount(0);
  }
});

test("reused native dialog discards drafts and credentials before reopening", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=profile-editor");
  const name = page.getByRole("textbox", { name: "Profile name" });
  await name.fill("Unsaved draft");
  await page.getByRole("tab", { name: "API key", exact: true }).click();
  await page.getByLabel("Phala AI API key").fill("sk-test-discard");
  await page.evaluate(() => window.dispatchEvent(new Event("mock:dialog-dismissed")));
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.evaluate(() => window.dispatchEvent(new Event("mock:dialog-open")));
  await expect(name).toHaveValue("Phala");
  await page.getByRole("tab", { name: "API key", exact: true }).click();
  await expect(page.getByLabel("Phala AI API key")).toHaveValue("");
  await name.fill("Another draft");
  await page.evaluate(() => {
    window.dispatchEvent(new Event("mock:dialog-dismissed"));
    window.dispatchEvent(new Event("mock:dialog-open"));
  });
  await expect(name).toHaveValue("Phala");
});

test("dialog theme is initialized before presentation without loading the chart engine", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.addInitScript(() => {
    if (new URLSearchParams(location.search).has("native-dialog")) window.__GATEWAY_INITIAL_APPEARANCE__ = "light";
    localStorage.setItem("pap-preview-appearance", "light");
  });
  const chartRequests: string[] = [];
  page.on("request", (request) => { if (request.url().includes("usage-chart-plot")) chartRequests.push(request.url()); });
  await page.goto("/?mock=appearance-pending&native-dialog=profile-editor");
  await expect(page.locator("html")).toHaveAttribute("data-native-presented", "true");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  expect(chartRequests).toEqual([]);
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-appearance")));
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");

  await page.goto("/?mock=appearance-pending");
  await expect(page.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
  await expect(page.locator("html")).not.toHaveAttribute("data-main-presented", "true");
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-appearance")));
  await expect(page.locator("html")).toHaveAttribute("data-main-presented", "true");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
});

test("native presentation waits for required credentials and local visual assets", async ({ page }) => {
  await page.goto("/?mock=example-key-pending&native-dialog=local-api-example");
  await expect(page.getByRole("dialog", { name: "Local API examples" })).toBeVisible();
  await expect(page.locator("html")).not.toHaveAttribute("data-native-presented", "true");
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-example-key")));
  await expect(page.locator("html")).toHaveAttribute("data-native-presented", "true");
  await expect(page.locator("code")).toContainText("sk-pap-");

  await page.addInitScript(() => {
    const decode = HTMLImageElement.prototype.decode;
    HTMLImageElement.prototype.decode = async function () {
      document.documentElement.dataset.imageDecodePending = "true";
      await new Promise<void>((resolve) => window.addEventListener("test:decode-images", () => resolve(), { once: true }));
      return decode.call(this);
    };
  });
  await page.goto("/?mock=ready&native-dialog=profile-editor");
  await expect(page.getByRole("dialog", { name: "New profile" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-image-decode-pending", "true");
  await expect(page.locator("html")).not.toHaveAttribute("data-native-presented", "true");
  await page.evaluate(() => window.dispatchEvent(new Event("test:decode-images")));
  await expect(page.locator("html")).toHaveAttribute("data-native-presented", "true");
});

test("complex dialogs render as native child-window surfaces", async ({ page }) => {
  await page.addInitScript(() => {
    new MutationObserver((mutations) => {
      for (const mutation of mutations) {
        for (const node of mutation.addedNodes) {
          if (node instanceof HTMLElement && /Loading (profiles|privacy verification|Local API settings|usage proof)/.test(node.textContent ?? "")) {
            document.documentElement.dataset.loadingFrameObserved = "true";
          }
        }
      }
    }).observe(document, { childList: true, subtree: true });
  });
  const cases = [
    {
      size: { width: 720, height: 560 },
      path: "/?mock=ready&native-dialog=local-api-example",
      name: "Local API examples",
      text: "sk-pap-",
    },
    {
      size: { width: 580, height: 560 },
      path: "/?mock=ready&native-dialog=profile-editor",
      name: "New profile",
      text: "Sign in with Phala",
    },
    {
      size: { width: 700, height: 680 },
      path: "/?mock=ready&native-dialog=privacy",
      name: "Privacy verification",
      text: "Attested encrypted channel",
    },
    {
      size: { width: 600, height: 512 },
      path: "/?mock=no-key&native-dialog=local-api",
      name: "Local API settings",
      text: "Listen address",
    },
    {
      size: { width: 560, height: 500 },
      path: "/?mock=ready&native-dialog=usage-proof&record=51be02",
      name: "Usage proof",
      text: "Signed receipt verified",
    },
    {
      size: { width: 620, height: 560 },
      path: "/?mock=ready&native-dialog=profiles",
      name: "Profiles",
      text: "New Profile",
    },
    {
      size: { width: 580, height: 580 },
      path: "/?mock=ready&native-dialog=notifications",
      name: "Notifications",
      text: "Allow notifications",
    },
  ] as const;

  for (const entry of cases) {
    await page.setViewportSize(entry.size);
    await page.goto(entry.path);
    await expect(page.locator(".desktop-window, .sidebar")).toHaveCount(0);
    const dialog = page.getByRole("dialog", { name: entry.name });
    await expect(dialog).toContainText(entry.text);
    await expect(page.locator("html")).toHaveAttribute("data-native-presented", "true");
    await expect(page.locator("html")).not.toHaveAttribute("data-loading-frame-observed", "true");
    expect(await dialog.boundingBox()).toEqual({ x: 0, y: 0, ...entry.size });
    expect(await dialog.evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
    await expect(dialog.locator(".sheet-footer")).toBeInViewport();
    if (entry.name === "New profile" || entry.name === "Local API settings") {
      expect(await dialog.locator(".sheet-scroll").evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
    }
    await expect(dialog.getByRole("heading").first()).toBeFocused();
    await expect(dialog.getByRole("heading").first()).toHaveCSS("outline-style", "none");
    expect(await dialog.getByRole("heading").first().evaluate((node) => getComputedStyle(node).boxShadow)).not.toMatch(/(?:^|\s)[1-9]\d*(?:\.\d+)?px/);
    if (entry.name === "Usage proof") {
      const alignment = await dialog.evaluate((node) => {
        const heading = node.querySelector(".sheet-heading")?.getBoundingClientRect();
        const body = node.querySelector(".proof-card")?.getBoundingClientRect();
        return [heading?.x, body?.x, heading?.right, body?.right];
      });
      expect(alignment[0]).toBe(alignment[1]);
      expect(alignment[2]).toBe(alignment[3]);
    }
    if (entry.name === "Usage proof" || entry.name === "Privacy verification") {
      const done = dialog.getByRole("button", { name: "Done", exact: true });
      await expect(done).toBeInViewport();
      const content = dialog.locator(entry.name === "Usage proof" ? ".proof-card" : ".privacy-content");
      expect(await content.evaluate((node) => node.scrollTop)).toBe(0);
      if (entry.name === "Privacy verification") {
        const identifiers = dialog.locator(".identity-grid strong.mono");
        expect(await identifiers.count()).toBeGreaterThan(0);
        expect(await identifiers.evaluateAll((nodes) => nodes.every((node) =>
          getComputedStyle(node).whiteSpace === "normal" && node.scrollWidth <= node.clientWidth,
        ))).toBe(true);
      }
      await content.evaluate((node) => { node.scrollTop = node.scrollHeight; });
      await expect(done).toBeInViewport();
      await expect(dialog.getByRole("list")).toHaveCount(0);
      if (entry.name === "Usage proof") {
        await expect(dialog.locator("svg")).toHaveCount(1);
        await expect(dialog.getByRole("region", { name: "Proof scope" })).toBeVisible();
      }
    }
    await page.setViewportSize({ width: entry.size.width, height: 420 });
    expect(await dialog.evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
    await expect(dialog.locator(".sheet-footer")).toBeInViewport();
  }
});

test("stale profile editors are dismissible and configuration verification cannot be cancelled", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=profile-editor&profile=deleted-profile");
  await expect(page.getByRole("alert")).toHaveText("This profile is no longer available.");
  await expect(page.getByRole("dialog", { name: "New profile" })).toHaveCount(0);
  await page.getByRole("button", { name: "Done" }).click();
  await expect(page.getByRole("main", { name: "Profiles closed" })).toBeVisible();

  await page.goto("/?mock=configuration-verifying");
  const control = page.getByRole("switch", { name: "Verifying configuration" });
  await expect(control).toBeDisabled();
  await expect(control).not.toBeChecked();
  await nav(page, "Settings").click();
  await expect(control).toBeDisabled();
});

test("usage query failures do not display stale totals or lose the selected filter", async ({ page }) => {
  await page.goto("/?mock=usage-query-error");
  await nav(page, "Usage").click();
  const history = page.getByRole("table", { name: "Usage history", exact: true });
  await expect(history.getByRole("button").first()).toBeVisible();
  const model = page.getByRole("combobox", { name: "Model", exact: true });
  await model.click();
  const selected = await page.getByRole("option").nth(1).innerText();
  await page.getByRole("option").nth(1).click();
  await expect(page.getByRole("alert")).toHaveText("Usage database temporarily unavailable");
  await expect(history.getByRole("button")).toHaveCount(0);
  await expect(page.locator(".usage-stats strong")).toHaveText(["—", "—", "—", "—"]);
  await expect(model.locator('[data-slot="select-value"]')).toHaveText(selected);
  await expect(page.getByText("Usage data unavailable.")).toBeVisible();

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

test("rotating the client key requires an explicit native confirmation", async ({ page }) => {
  await page.goto("/?mock=interactive&native-dialog=local-api");
  const key = page.getByLabel("Client key", { exact: true });
  await expect(key).not.toHaveValue("");
  await expect(page.locator(".sheet-card")).toHaveCount(0);
  const keyGroup = page.locator('[data-slot="input-group"]', { has: key });
  await expect(keyGroup).toHaveCSS("height", "36px");
  await expect(keyGroup).toHaveCSS("border-radius", "26px");
  await expect(keyGroup.getByRole("button")).toHaveCount(3);
  await keyGroup.getByRole("button", { name: "Reveal client key" }).click();
  await expect(key).toHaveAttribute("type", "text");
  await keyGroup.getByRole("button", { name: "Hide client key" }).click();
  await expect(key).toHaveAttribute("type", "password");
  const listen = await page.getByLabel("Listen address", { exact: true }).boundingBox();
  const port = await page.getByLabel("Port", { exact: true }).boundingBox();
  expect(listen?.y).toBe(port?.y);
  await expect(page.getByRole("switch", { name: "Allow network access" })).toHaveCount(0);
  await page.getByLabel("Listen address", { exact: true }).fill("192.168.1.20");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("button", { name: "Network access warning" })).toBeVisible();
  await page.getByLabel("Listen address", { exact: true }).fill("127.0.0.1");
  await page.keyboard.press("Escape");
  const original = await key.inputValue();
  page.once("dialog", (dialog) => dialog.dismiss());
  await page.getByRole("button", { name: "Rotate key" }).click();
  await expect(key).toHaveValue(original);
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Rotate key" }).click();
  await expect(key).not.toHaveValue(original);
  for (const surface of ["&native-dialog=local-api", ""]) {
    await page.goto(`/?mock=key-rotation-error${surface}`);
    if (!surface) await page.getByRole("button", { name: "Local API settings", exact: true }).click();
    await expect(key).not.toHaveValue("");
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Rotate key" }).click();
    await expect(key).toHaveValue("");
    await expect(page.getByRole("button", { name: "Copy client key" })).toBeDisabled();
    await expect(page.getByRole("button", { name: "Rotate key" })).toBeEnabled();
    await expect(page.getByRole("alert")).toContainText("Could not store the replacement client key");
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Rotate key" }).click();
    await expect(key).toHaveValue(/^sk-pap-/);
    await expect(page.getByRole("button", { name: "Copy client key" })).toBeEnabled();
    await expect(page.getByRole("alert")).toHaveCount(0);
  }
});

test("protection flow, page headers, and focus follow the native desktop contract", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=no-profiles");

  await expect(page).toHaveTitle("Private AI Proxy");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.locator(".status-heading")).toHaveCSS("color", await themeColor(page, "--muted-foreground"));
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);
  let editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toHaveCount(0);
  await page.getByRole("switch", { name: "Start protection" }).click();
  editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toBeVisible();
  await expect(editor.getByRole("button", { name: "Phala", exact: true })).toHaveAttribute("aria-pressed", "true");
  await editor.getByRole("tab", { name: "API key", exact: true }).click();
  await editor.getByLabel("Phala AI API key").fill("sk-test-123");
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);

  await expect(page.getByLabel("Protection status").getByText("Protected", { exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();

  await nav(page, "Overview").focus();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Agents")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Usage")).toBeFocused();

  await nav(page, "Settings").click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();
  await expect(page.getByRole("button", { name: "Profiles", exact: true })).toContainText("Protected");
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();

  await nav(page, "Overview").click();
  await expect(page.getByRole("heading", { name: "Overview", level: 1 })).toBeFocused();
  await expect(page.getByLabel("Protection status").getByText("Protected", { exact: true })).toBeVisible();
  await expect(page.locator(".tracks-left")).toHaveCount(0);
  await expect(page.locator(".tracks-right")).toHaveCSS("opacity", "1");

  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.locator(".tracks-right")).toHaveCSS("opacity", "0");
});

test("agent icons remain visible in both themes without connection-state fading", async ({ page }) => {
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await page.goto("/?mock=all-agent-icons");
    await nav(page, "Agents").click();
    const rows = page.locator(".agent-block");
    const icons = rows.locator(".mark img");
    await expect(icons).toHaveCount(7);
    await expect.poll(() => icons.evaluateAll((images) =>
      images.every((image) => image instanceof HTMLImageElement && image.complete && image.naturalWidth > 0 && getComputedStyle(image).opacity === "1"),
    )).toBe(true);
    await expect(rows.filter({ hasText: "OpenClaw" }).locator("img")).toHaveAttribute("src", /openclaw-color/);
    await expect(rows.filter({ hasText: "Oh My Pi" }).locator(".mark")).toHaveCSS("background-color", "rgb(13, 13, 13)");
  }
});

test("five agents connect and disconnect directly from the verified discovered catalog", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");
  await nav(page, "Agents").click();

  const rows = page.locator(".agent-block");
  await expect(rows).toHaveCount(5);
  for (const name of ["Codex", "Claude Code", "OpenCode", "Pi", "Hermes"]) {
    await expect(rows.filter({ hasText: name })).toBeVisible();
  }
  const agentImageElements = rows.locator(".mark img");
  await expect(agentImageElements).toHaveCount(5);
  await expect.poll(() => agentImageElements.evaluateAll((images) =>
    images.every((image) => image instanceof HTMLImageElement && image.complete && image.naturalWidth > 0),
  )).toBe(true);
  const iconResults = await agentImageElements.evaluateAll((images) =>
    images.map((image) => ({ source: (image as HTMLImageElement).currentSrc })),
  );
  expect(iconResults).toHaveLength(5);
  expect(iconResults.every(({ source }) => source.includes("/assets/") && !source.startsWith("data:"))).toBe(true);

  const codex = rows.filter({ hasText: "Codex" });
  await codex.getByRole("switch", { name: "Connect Codex" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(codex.getByText("Connected", { exact: true })).toBeVisible();

  const pi = rows.filter({ hasText: "Pi" });
  await pi.getByRole("switch", { name: "Connect Pi" }).click();
  await expect(pi.getByText("Connected", { exact: true })).toBeVisible();
  await pi.getByRole("switch", { name: "Disconnect Pi" }).click();
  await expect(pi.getByText("Not connected", { exact: true })).toBeVisible();

  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  page.once("dialog", async (dialog) => {
    expect(dialog.type()).toBe("confirm");
    expect(dialog.message()).toContain("Reset settings?");
    await dialog.accept();
  });
  await page.getByRole("button", { name: "Reset settings", exact: true }).click();
  await expect(page.getByRole("heading", { name: "Settings", exact: true })).toBeVisible();
  await nav(page, "Agents").click();
  await expect(page.getByRole("switch", { name: /^Disconnect / })).toHaveCount(0);
});

test("overview shows three agents, current-session records, truthful copy surfaces, and session totals", async ({ page }) => {
  await page.setViewportSize({ width: 1100, height: 1040 });
  await page.goto("/?mock=ready");

  const agentsModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents" }) });
  await expect(agentsModule.locator(".agent-block")).toHaveCount(3);
  await expect(agentsModule.locator(".agent-block").last()).toBeVisible();
  const usageModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Recent usage" }) });
  await expect(usageModule.locator(".usage-row")).toHaveCount(5);
  await expect(usageModule.locator(".usage-row").last()).toBeVisible();
  expect(await agentsModule.locator(".module").evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true);
  expect(await usageModule.locator(".module").evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true);
  expect(await page.locator(".content").evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
  const spacing = await page.locator(".overview-page").evaluate((node) => {
    const modules = Array.from(node.querySelectorAll(".overview-module"), (item) => item.getBoundingClientRect());
    const surface = node.querySelector(".status-surface")?.getBoundingClientRect();
    if (!surface || modules.length !== 3) throw new Error("Overview modules missing");
    return [modules[0].top - surface.bottom, modules[1].top - modules[0].bottom];
  });
  expect(Math.abs(spacing[0] - spacing[1])).toBeLessThanOrEqual(1);
  await expect(page.locator(".overview-module-title").first()).toHaveCSS("user-select", "none");
  await usageModule.locator(".usage-row").first().click();
  const overviewProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(overviewProof).toContainText("Signed receipt verified");
  await overviewProof.getByRole("button", { name: "Done" }).click();

  const session = page.getByRole("region", { name: "Current session", exact: true });
  const localApi = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Local API" }) });
  for (const label of ["Requests", "Tokens", "Estimated cost"]) {
    await expect(session.getByText(label, { exact: true })).toBeVisible();
  }
  await expect(session.locator("small")).toHaveCount(0);
  await expect(session.getByText("This session", { exact: true })).toHaveCount(0);
  await expect(localApi.locator(".overview-module-title").getByText("Available", { exact: true })).toBeVisible();
  await expect(localApi.locator(".copy-rows").getByText("Available", { exact: true })).toHaveCount(0);
  await expect(localApi.getByText("for your own tools", { exact: true })).toHaveCount(0);
  await expect(localApi.locator('[data-slot="item"][data-variant="muted"]')).toHaveCount(2);
  await expect(session.locator('[data-slot="session-metric"]')).toHaveCount(3);

  const endpoint = localApi.getByRole("button", { name: /Local endpoint/ });
  await endpoint.hover();
  await expect(endpoint.getByText("Copy", { exact: true })).toBeVisible();
  await endpoint.getByText("http://127.0.0.1:4180").click();
  await expect(endpoint.getByText("Copied", { exact: true })).toBeVisible();
  await expect(page.locator('.sr-only[role="status"]')).toContainText("Local endpoint copied");

  const clientKey = localApi.getByRole("button", { name: /Client key/ });
  await clientKey.click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText("Client key copied");
  await localApi.getByRole("button", { name: "Reveal client key" }).click();
  await expect(clientKey).toContainText("sk-pap-");

  await localApi.getByRole("button", { name: "Local API settings" }).click();
  const localSheet = page.getByRole("dialog", { name: "Local API settings" });
  await expect(localSheet).toBeVisible();
  for (const label of ["Listen address", "Port", "Client host", "Client key"]) {
    await expect(localSheet.getByText(label, { exact: true }).first()).toBeVisible();
  }
  await expect(localSheet.getByRole("button", { name: /Copy .*endpoint/ })).toHaveCount(0);
  await expect(localSheet.getByRole("group", { name: "Client endpoints", exact: true })).toHaveCount(0);
  await expect(localSheet.locator('[data-slot="field-separator"]')).toHaveCount(1);
  await expect(localSheet.locator('.sheet-footer > [data-slot="separator"]')).toHaveCSS("height", "1px");
  await localSheet.getByLabel("Listen address", { exact: true }).fill("192.168.1.20");
  await page.keyboard.press("Escape");
  await localSheet.getByRole("button", { name: "Network access warning" }).hover();
  await expect(page.getByRole("tooltip").and(page.locator("[data-open]"))).toContainText("trusted network");
  await localSheet.getByLabel("Listen address", { exact: true }).fill("127.0.0.1");
  await page.keyboard.press("Escape");
  await expect(localSheet.getByText("Access keys", { exact: true })).toHaveCount(0);
  await expect(localSheet.getByRole("button", { name: "Save" })).toBeEnabled();
  await expect(localSheet).not.toContainText("Saving briefly");
  await localSheet.getByLabel("Port", { exact: true }).fill("4181");
  await localSheet.getByRole("button", { name: "Save", exact: true }).click();
  await expect(localSheet).not.toBeVisible();
  await expect(endpoint).toContainText("4181");
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(session.locator("strong")).toHaveText(["—", "—", "—"]);
  await expect(usageModule.locator(".usage-row")).toHaveCount(0);
  await expect(page.locator(".protection-duration")).toHaveCount(0);
});

test("updates are discovered on launch and installation requires confirmation", async ({ page }) => {
  await page.goto("/?mock=update-available");
  const updateBadge = page.getByRole("button", { name: "Update available", exact: true });
  await expect(updateBadge).toHaveAttribute("data-slot", "badge");
  await expect(updateBadge).toHaveAttribute("data-variant", "outline");
  await expect(updateBadge).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
  await expect(updateBadge).toHaveCSS("height", "32px");
  await expect(updateBadge).toHaveCSS("font-size", "14px");
  await expect(updateBadge.locator("svg")).toHaveCSS("width", "16px");
  const badgeBounds = await updateBadge.boundingBox();
  const navigationBounds = await page.getByRole("navigation", { name: "Main navigation" }).boundingBox();
  expect(badgeBounds).not.toBeNull();
  expect(navigationBounds).not.toBeNull();
  expect(badgeBounds?.x).toBe(navigationBounds?.x);
  expect(badgeBounds?.width).toBe(navigationBounds?.width);
  const bottomGap = await updateBadge.evaluate((badge) => {
    const sidebar = badge.closest("aside");
    if (!sidebar) throw new Error("Update badge must be inside the sidebar");
    return sidebar.getBoundingClientRect().bottom - badge.getBoundingClientRect().bottom;
  });
  expect(bottomGap).toBeGreaterThanOrEqual(10);
  expect(bottomGap).toBeLessThanOrEqual(16);
  let sidebarConfirmation = "";
  page.once("dialog", async (dialog) => {
    sidebarConfirmation = dialog.message();
    await dialog.dismiss();
  });
  await updateBadge.click();
  await expect(page.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
  await nav(page, "Settings").click();
  await expect(page.getByRole("status").filter({ hasText: "Version 0.2.0 is available" })).toBeVisible();
  page.once("dialog", async (dialog) => {
    expect(dialog.message()).toBe(sidebarConfirmation);
    expect(dialog.message()).toContain("connected agent configurations will be restored");
    await dialog.dismiss();
  });
  await page.getByRole("button", { name: "Install and Restart", exact: true }).click();
  await expect(page.getByRole("button", { name: "Install and Restart", exact: true })).toBeEnabled();
  await expect(page.getByRole("heading", { name: "Updates", exact: true })).toHaveCount(0);
  const about = page.getByRole("region", { name: "About", exact: true });
  await expect(about.locator('[data-slot="app-version"]')).toHaveText("v0.1.0");
  await expect(page.getByRole("button", { name: "Check for Updates" })).toHaveCount(0);
  await expect(about.getByRole("button", { name: "Documentation", exact: true })).toHaveCSS("border-bottom-width", "0px");
  await expect(about.getByRole("button", { name: "GitHub", exact: true })).toHaveAttribute("data-slot", "item");
  await expect(about.getByRole("combobox", { name: "Update channel" })).toHaveCount(0);
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  const channel = page.locator(".settings-advanced").getByRole("group", { name: "Update channel" });
  await expect(channel.getByRole("button", { name: "Stable" })).toHaveAttribute("aria-pressed", "true");
  await channel.getByRole("button", { name: "Beta" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Version 0.3.0-beta.1 is available" })).toBeVisible();
  await nav(page, "Overview").click();
  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  await expect(channel.getByRole("button", { name: "Beta" })).toHaveAttribute("aria-pressed", "true");
  await channel.getByRole("button", { name: "Stable" }).click();
  await expect(page.getByRole("status").filter({ hasText: "Version 0.2.0 is available" })).toBeVisible();
});

test("success colors, list separators, control sizes and About alignment are consistent", async ({ page }) => {
  await page.setViewportSize({ width: 1052, height: 752 });
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await page.goto("/?mock=ready");
    const success = await themeColor(page, "--primary");
    for (const action of await page.getByRole("button", { name: "View all", exact: true }).all()) {
      await expect(action).toHaveCSS("color", await themeColor(page, "--foreground"));
    }
    const local = page.locator(".overview-module-title", { has: page.getByRole("heading", { name: "Local API", exact: true }) });
    const available = local.locator('[data-slot="badge"]').filter({ hasText: /^Available$/ });
    await expect(available).toHaveCSS("color", await themeColor(page, "--muted-foreground"));
    await expect(available).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
    await expect(available.locator('[data-slot="status-dot"]')).toHaveCSS("background-color", success);
    await expect(nav(page, "Agents")).toHaveCSS("border-radius", "14px");
    const protectionShadow = await page.locator(".status-compact").evaluate((node) => getComputedStyle(node).boxShadow);
    expect(protectionShadow).toContain(success);
    expect(protectionShadow).toContain("4px 6px -1px");
    await expect(page.locator(".status-compact")).toHaveClass(/shadow-primary\/10/);
    await expect(page.getByLabel("Protection status").getByRole("switch")).toHaveCSS("width", "44px");
    await expect(page.getByLabel("Protection status").getByRole("switch")).toHaveCSS("height", "20px");
    await expect(page.getByLabel("Protection status").getByRole("switch")).toHaveCSS("background-color", success);
    await expect(page.getByRole("button", { name: "Profiles: RedPill" })).toHaveCSS("width", "140px");
    await expect(nav(page, "Agents")).toHaveCSS("height", "36px");
    await expect(nav(page, "Agents")).toHaveCSS("font-weight", "400");
    const buttonBefore = await nav(page, "Agents").boundingBox();
    await nav(page, "Agents").click();
    await expect(nav(page, "Agents")).toHaveCSS("font-weight", "500");
    expect(await nav(page, "Agents").boundingBox()).toEqual(buttonBefore);
    const installed = page.getByRole("region", { name: /^Installed/ });
    const separators = installed.locator('[data-slot="separator"]:visible');
    expect(await separators.count()).toBe((await installed.locator(".agent-block").count()) - 1);
    await expect(installed.locator('[data-slot="badge"]', { hasText: /^Connected$/ }).first().locator('[data-slot="status-dot"]')).toHaveCSS("background-color", success);
    await expect(page.locator(".page-header").getByRole("switch")).toHaveCSS("width", "44px");
    await expect(page.locator(".page-header").getByRole("switch")).toHaveCSS("background-color", success);
    await nav(page, "Usage").click();
    await expect(page.locator(".page-header").getByRole("switch")).toHaveCSS("background-color", success);
    await nav(page, "Settings").click();
    await expect(page.locator(".page-header").getByRole("switch")).toHaveCSS("background-color", success);
    const general = page.getByRole("region", { name: "General", exact: true });
    await expect(general.locator('[data-slot="separator"]')).toHaveCount(3);
    const generalRows = general.locator('[data-slot="item-group"] > [data-slot="item"]');
    expect(await generalRows.evaluateAll((rows) => rows.map((row) => row.getBoundingClientRect().height))).toEqual([52, 52, 52, 52]);
    await expect(general.locator('[data-slot="item-group"]')).toHaveCSS("row-gap", "0px");
    await expect(general.locator('[data-slot="item-group"]')).toHaveCSS("background-color", await themeColor(page, "--card"));
    await expect(general.locator('[data-slot="item-group"]')).toHaveCSS("padding", "0px");
    await expect(general.locator('[data-slot="separator"]').first()).toHaveCSS("height", "1px");
    const about = page.getByRole("region", { name: "About", exact: true });
    await expect(about.getByRole("status")).toHaveText("You're up to date");
    const aligned = await about.evaluate((node) => {
      const version = node.querySelector('[data-slot="app-version"]')?.getBoundingClientRect();
      const status = node.querySelector('[role="status"]')?.getBoundingClientRect();
      if (!version || !status) throw new Error("Missing About metadata");
      return Math.abs(version.y + version.height / 2 - status.y - status.height / 2);
    });
    expect(aligned).toBeLessThanOrEqual(1);
    await expect(about.getByRole("status")).toHaveCSS("text-align", "right");
  }
});

test("availability and connection badges render visible status dots", async ({ page }) => {
  await page.goto("/?mock=ready");
  for (const label of ["Available", "Connected", "Not connected"]) {
    const badge = page.locator('[data-slot="badge"]').filter({ hasText: new RegExp(`^${label}$`) }).first();
    const dot = badge.locator('[data-slot="status-dot"]');
    await expect(dot).toHaveCSS("width", "6px");
    await expect(dot).toHaveCSS("height", "6px");
    await expect(badge).toHaveCSS("color", await themeColor(page, "--muted-foreground"));
    await expect(badge).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
    await expect(dot).toHaveCSS("background-color", await themeColor(page, label === "Not connected" ? "--muted-foreground" : "--primary"));
  }
  await page.getByRole("switch", { name: "Stop protection", exact: true }).click();
  const unavailable = page.locator('[data-slot="badge"]').filter({ hasText: /^Unavailable$/ });
  await expect(unavailable.locator('[data-slot="status-dot"]')).toHaveCSS("width", "6px");
});

test("agent actions report progress without disabling unrelated switches", async ({ page }) => {
  await page.addInitScript(() => window.addEventListener("mock:agent-write", () => {
    document.documentElement.dataset.agentWrites = String(Number(document.documentElement.dataset.agentWrites ?? 0) + 1);
  }));
  await page.goto("/?mock=agent-pending");
  await nav(page, "Agents").click();
  await expect(page.getByRole("button", { name: "Detect installed agents" })).toHaveCount(0);
  await expect(page.locator('.agent-block [data-slot="item-description"]')).toHaveCount(0);
  await page.getByRole("switch", { name: "Connect Codex", exact: true }).click();
  const pending = page.getByRole("switch", { name: "Disconnect Codex", exact: true });
  await expect(pending).toBeChecked();
  await expect(pending).toBeEnabled();
  await expect(pending).toHaveAttribute("aria-busy", "true");
  await expect(page.getByRole("switch", { name: "Connect Pi", exact: true })).toBeEnabled();
  await pending.click();
  const reversed = page.getByRole("switch", { name: "Connect Codex", exact: true });
  await expect(reversed).not.toBeChecked();
  await expect(reversed).toHaveAttribute("aria-busy", "true");
  await reversed.click();
  await pending.click();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-agent")));
  await expect(reversed).toHaveAttribute("aria-busy", "false");
  await expect(reversed).not.toBeChecked();
  await expect(page.locator("html")).toHaveAttribute("data-agent-writes", "2");
});

test("update installation uses a progress dialog and exposes failure without a fake cancel", async ({ page }) => {
  await page.goto("/?mock=update-install-error");
  await nav(page, "Settings").click();
  const about = page.getByRole("region", { name: "About", exact: true, includeHidden: true });
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Update available", exact: true }).click();
  const progress = page.getByRole("dialog", { name: "Installing update", exact: true });
  await expect(progress).toBeVisible();
  await expect(progress.getByRole("progressbar", { name: "Update progress" })).toHaveAttribute("aria-valuenow", "40");
  await expect(about).not.toContainText("Downloading");
  await expect(about).not.toContainText("Preparing update");
  await page.keyboard.press("Escape");
  await expect(progress).toBeVisible();
  await expect(progress.getByRole("button", { name: "Cancel" })).toHaveCount(0);
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-update")));
  const failure = page.getByRole("dialog", { name: "Update failed", exact: true });
  await expect(failure).toBeVisible();
  await expect(about).not.toContainText("installation failed");
  await failure.getByRole("button", { name: "Done" }).click();
  await expect(failure).toHaveCount(0);
  await expect(about.getByRole("button", { name: "Install and Restart" })).toBeEnabled();
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

test("settings keep the installed version visible without manual update controls", async ({ page }) => {
  for (const [scenario, message] of [
    ["update-unpublished", "No releases published in this channel yet"],
    ["update-offline", "Could not check for updates. Retrying automatically."],
  ]) {
    await page.goto(`/?mock=${scenario}`);
    await nav(page, "Settings").click();
    const about = page.getByRole("region", { name: "About", exact: true });
    await expect(about.locator('[data-slot="app-version"]')).toHaveText("v0.1.0");
    await expect(about.getByRole("status")).toHaveText(message);
    await expect(about.getByRole("button", { name: /Updates|Install and Restart/ })).toHaveCount(0);
    await expect(about.getByRole("combobox")).toHaveCount(0);
    await page.getByRole("button", { name: "Advanced", exact: true }).click();
    const advanced = page.locator(".settings-advanced");
    await expect(advanced.getByRole("group", { name: "Update channel" })).toBeVisible();
    const padding = await advanced.locator('[data-slot="item"]').first().evaluate((item) => getComputedStyle(item).paddingLeft);
    expect(Number.parseFloat(padding)).toBeGreaterThanOrEqual(12);
  }
  await page.goto("/?mock=update-recover");
  await nav(page, "Settings").click();
  const status = page.getByRole("region", { name: "About", exact: true }).getByRole("status");
  await expect(status).toContainText("Retrying automatically");
  await page.evaluate(() => window.dispatchEvent(new Event("online")));
  await expect(status).toHaveText("You're up to date");
});

test("local copy hover follows the grouped row shape and profiles open their dialog", async ({ page }) => {
  await page.goto("/?mock=ready");
  const copy = page.getByRole("button", { name: /^Local endpoint:/ });
  await copy.hover();
  await expect(copy).toHaveCSS("border-radius", "0px");
  const shape = await copy.evaluate((button) => {
    const row = button.parentElement;
    const group = button.closest(".copy-row");
    if (!row || !group) throw new Error("Copy row structure missing");
    return { height: button.getBoundingClientRect().height, rowHeight: row.clientHeight, clipped: getComputedStyle(group).overflow, radius: getComputedStyle(group).borderRadius };
  });
  expect(Math.abs(shape.height - shape.rowHeight)).toBeLessThanOrEqual(1);
  expect(shape.clipped).toBe("hidden");
  expect(Number.parseFloat(shape.radius)).toBeGreaterThan(0);
  for (const name of ["Local API settings", "Reveal client key"]) {
    const button = page.getByRole("button", { name, exact: true });
    await button.hover();
    const before = await button.boundingBox();
    if (!before) throw new Error("Missing Local API button");
    await page.mouse.down();
    try {
      // Let the real pressed transition finish while the pointer stays down.
      await button.evaluate(async (node) => {
        await Promise.allSettled(node.getAnimations().map((animation) => animation.finished));
      });
      const pressed = await button.boundingBox();
      if (!pressed) throw new Error("Missing pressed Local API button");
      expect(Math.abs(pressed.y - before.y)).toBeLessThanOrEqual(1);
      expect(pressed.x).toBe(before.x);
      await page.mouse.move(0, 0);
    } finally {
      await page.mouse.up();
    }
  }
  await page.getByRole("button", { name: "Reveal client key", exact: true }).click();
  await expect(page.getByRole("button", { name: "Hide client key", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Local API settings", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Local API settings", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  const profile = page.getByRole("button", { name: "Profiles: RedPill" });
  await expect(profile).toHaveAttribute("aria-haspopup", "dialog");
  await profile.click();
  await expect(page.getByRole("dialog", { name: "Profiles", exact: true })).toBeVisible();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(profile).toBeFocused();
});

test("usage history filters, paginates and inspects proof boundaries", async ({ page }) => {
  await page.clock.setFixedTime(new Date("2026-09-06T12:00:00"));
  await page.setViewportSize({ width: 940, height: 760 });
  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();
  const tableContainer = page.locator('.usage-history [data-slot="table-container"]');
  await expect(tableContainer).toHaveCSS("border-width", "0px");
  await expect(tableContainer).toHaveCSS("border-radius", "0px");
  await expect(tableContainer.getByRole("button").first()).toHaveCSS("color", await themeColor(page, "--foreground"));

  await expect(page.locator('[data-slot="chart"] .recharts-surface')).toBeVisible();
  await expect(page.getByRole("table", { name: "Usage by model", includeHidden: true }).locator("tbody tr")).toHaveCount(7);
  const chartDays = await page.getByRole("table", { name: "Usage by model", includeHidden: true }).locator("tbody th").allTextContents(
  );
  expect(new Set(chartDays).size).toBe(7);

  await page.getByRole("button", { name: /^Date range:/ }).click();
  await choose(page, page.getByRole("combobox", { name: "Quick date range" }), "Today");
  await expect(page.getByRole("table", { name: "Usage by model", includeHidden: true }).locator("tbody tr")).toHaveCount(1);
  await expect.poll(async () => page.locator('.usage-history time').evaluateAll((times) => times.length > 0 && times.every((time) => {
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    return new Date(time.getAttribute("datetime") ?? "").getTime() >= midnight.getTime();
  }))).toBe(true);
  await page.getByRole("button", { name: /^Date range:/ }).click();
  await choose(page, page.getByRole("combobox", { name: "Quick date range" }), "Last 7 days");

  const metric = page.getByRole("tablist", { name: "Chart metric" });
  await metric.getByRole("tab", { name: "Tokens", exact: true }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(metric.getByRole("tab", { name: "Cost", exact: true })).toBeFocused();
  await page.keyboard.press("Space");
  await expect(metric.getByRole("tab", { name: "Cost", exact: true })).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Space");
  await expect(metric.getByRole("tab", { name: "Cost", exact: true })).toHaveAttribute("aria-selected", "true");

  const history = page.getByRole("table", { name: "Usage history", exact: true });
  await expect(history.locator("tbody tr")).toHaveCount(20);
  await expect(history.getByRole("columnheader")).toHaveText(["Time", "Agent", "Model", "Tokens", "Cost", "Result"]);
  await history.getByLabel("Token details", { exact: true }).first().hover();
  await expect(page.getByRole("tooltip").and(page.locator("[data-open]"))).toContainText("Cache read");
  await page.keyboard.press("Escape");
  await choose(page, page.getByRole("combobox", { name: "Rows per page" }), "50");
  await expect(history.locator("tbody tr")).not.toHaveCount(20);
  await expect(page.getByRole("button", { name: "Next usage page" })).toBeDisabled();
  await choose(page, page.getByRole("combobox", { name: "Rows per page" }), "20");
  await page.getByRole("button", { name: "Next usage page" }).click();
  await expect(page.getByRole("heading", { name: "Usage history" })).toBeFocused();
  await expect(page.locator(".pagination").getByText(/^Page 2/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Previous usage page" })).toBeEnabled();

  const uncertainDelivery = history.getByRole("button", { name: /Upstream failed/ }).first();
  await uncertainDelivery.click();
  const uncertainProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(uncertainProof).toContainText("whether the service received it could not be confirmed");
  await uncertainProof.getByRole("button", { name: "Done" }).click();

  const agentFilter = page.getByRole("combobox", { name: "Agent", exact: true });
  await choose(page, agentFilter, "Hermes Agent");
  await expect(history.getByRole("button").first()).toContainText("Hermes");
  await choose(page, agentFilter, "All agents");
  const blocked = history.getByRole("button", { name: /Blocked locally/ }).first();
  await blocked.click();
  const blockedProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(blockedProof.getByText("Blocked locally", { exact: true })).toBeVisible();
  await expect(blockedProof.getByText(/did not leave this Mac/)).toBeVisible();
  await expect(blockedProof.getByText("Request kept on this Mac", { exact: true })).toBeVisible();
  await expect(blockedProof.locator(".proof-flow, .privacy-verdict.state-success")).toHaveCount(0);
  await blockedProof.getByRole("button", { name: "Done" }).click();
  await expect(history).not.toContainText("/v1/models");

  await expect(page.getByRole("button", { name: "Export usage as CSV" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Clear usage history" })).toHaveCount(0);
});

test("service settings stay focused while privacy verification exposes the complete proof", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();
  await page.getByRole("switch", { name: "Stop protection" }).click();

  await expect(page.locator("details.model-catalog")).toHaveCount(0);
  await page.getByRole("button", { name: "Profiles", exact: true }).click();
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  await expect(profiles.getByText("Model catalog", { exact: true })).toHaveCount(0);
  await expect(profiles.getByText(/Ready/)).toBeVisible();
  const redpillLogo = profiles.locator(".service-redpill img");
  await expect(redpillLogo).toBeVisible();
  await expect.poll(() => redpillLogo.evaluate((image) => (image as HTMLImageElement).currentSrc)).toContain("service-redpill-");
  expect(await redpillLogo.evaluate((image) => (image as HTMLImageElement).currentSrc)).toContain(".png");
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  await expect(page.getByRole("dialog", { name: "Edit profile" }).getByText("Verified configuration", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Delete credential", exact: true })).toHaveCount(0);
  await page.getByRole("dialog", { name: "Edit profile" }).getByRole("button", { name: "Cancel" }).click();
  await profiles.getByRole("button", { name: "Done" }).click();
  await page.getByRole("switch", { name: "Start protection" }).click();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();
  await nav(page, "Overview").click();
  await page.getByRole("button", { name: "Privacy verification", exact: true }).click();
  const privacy = page.getByRole("dialog", { name: "Privacy verification" });
  await expect(privacy.getByText("Attested encrypted channel")).toBeVisible();
  await expect(privacy).toContainText("SPKI-pinned TLS");
  await expect(privacy.locator("details")).toHaveCount(0);
  await expect(privacy.getByRole("heading", { name: "Verification checks" })).toBeVisible();
});

test("overview presents local availability and the active profile without session filler", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");

  const status = page.getByLabel("Protection status");
  await expect(status.locator(".status-local")).toHaveCount(0);
  await expect(status.getByText("Protected", { exact: true })).toBeVisible();
  const localHeader = page.locator(".overview-module-title").filter({ has: page.getByRole("heading", { name: "Local API", exact: true }) });
  const badgeOffset = await localHeader.evaluate((header) => {
    const title = header.querySelector("h2")?.getBoundingClientRect();
    const badge = header.querySelector('[data-slot="badge"]')?.getBoundingClientRect();
    if (!title || !badge) throw new Error("Missing Local API heading or status");
    return Math.abs(title.y + title.height / 2 - badge.y - badge.height / 2);
  });
  expect(badgeOffset).toBeLessThanOrEqual(0.5);
  await expect(status.getByRole("button", { name: "Profiles: RedPill" })).toBeVisible();
  await expect(status.locator(".status-endpoint")).toHaveCount(0);
  await expect(status.locator(".protection-duration")).toHaveText(/00:10:\d{2}/);
  const alignment = await status.evaluate((node) => {
    const verified = node.querySelector('[aria-label="Privacy verification"]')?.getBoundingClientRect();
    const profile = node.querySelector(".status-profile")?.getBoundingClientRect();
    const toggle = node.querySelector('[role="switch"]')?.getBoundingClientRect();
    const heading = node.querySelector(".status-heading")?.getBoundingClientRect();
    return { leftEdges: [heading?.left, profile?.left], positions: [toggle?.top ?? 0, profile?.top ?? 0], bottomEdges: [verified?.bottom, profile?.bottom], infoGap: (verified?.left ?? 0) - (profile?.right ?? 0) };
  });
  expect(new Set(alignment.leftEdges).size).toBe(1);
  expect(alignment.positions[0]).toBeLessThan(alignment.positions[1]);
  expect(new Set(alignment.bottomEdges).size).toBe(1);
  expect(alignment.infoGap).toBe(8);
  await status.getByRole("button", { name: "Privacy verification", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Privacy verification" })).toBeVisible();
  await page.getByRole("dialog", { name: "Privacy verification" }).getByRole("button", { name: "Done", exact: true }).click();
  await expect(status.getByText(/answers this session/i)).toHaveCount(0);

  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(status.getByRole("button", { name: "Privacy verification", exact: true })).toBeVisible();
  await expect(status.getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.locator(".tray-template-icon")).toHaveCSS("opacity", "0.45");
  for (const name of ["Agents", "Usage", "Settings"]) {
    await nav(page, name).click();
    await expect(page.locator(".page-switch-copy")).toHaveCSS("color", await themeColor(page, "--muted-foreground"));
  }
  await expect(page.getByRole("button", { name: "Profiles", exact: true })).toHaveCSS("border-bottom-width", "0px");
  await nav(page, "Agents").click();
  await expect(page.getByRole("button", { name: "Detect installed agents" })).toHaveCount(0);

  await page.goto("/?mock=no-key");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await page.getByRole("switch", { name: "Start protection" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor).toBeVisible();
  await expect(editor.getByLabel("RedPill API key")).toBeVisible();
});

test("installed agents stay ordered and protection state is consistent across pages", async ({ page }) => {
  await page.goto("/?mock=mixed-agents");
  await expect(page.locator(".status-agent-icon")).toHaveCount(0);
  const preview = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents", exact: true }) });
  await expect(preview.locator(".agent-block")).toHaveCount(3);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  const installed = page.getByRole("region", { name: /^Installed/ });
  await expect(installed.getByRole("heading")).not.toContainText("active");
  const centered = await page.locator(".agent-block .row-title-line").evaluateAll((rows) => rows.every((row) => {
    const name = row.children[0].getBoundingClientRect();
    const status = row.children[1].getBoundingClientRect();
    return Math.abs(name.top + name.height / 2 - status.top - status.height / 2) <= 1;
  }));
  expect(centered).toBe(true);
  const order = await installed.locator(".row-title").allTextContents();
  await installed.getByRole("switch", { name: "Connect Codex" }).click();
  await expect(installed.locator(".row-title")).toHaveText(order);
  const absent = page.getByRole("region", { name: "Not installed" });
  await expect(absent.getByRole("switch")).toHaveCount(0);
  await expect(absent.getByRole("button", { name: "Website" })).toBeVisible();
  const header = page.locator(".page-header");
  await expect(header.getByText("Protected", { exact: true })).toBeVisible();
  const label = await header.locator(".protection-status > span").boundingBox();
  const timer = await header.locator(".protection-duration").boundingBox();
  expect(label && timer && timer.y >= label.y + label.height).toBeTruthy();
  await header.getByRole("switch", { name: "Stop protection" }).click();
  await expect(header.getByText("Not protected", { exact: true })).toBeVisible();
  await expect(header.locator(".protection-duration")).toHaveCount(0);
  await page.getByRole("button", { name: "Overview", exact: true }).click();
  await expect(page.locator(".overview-module-title").filter({ has: page.getByRole("heading", { name: "Local API", exact: true }) })).toContainText("Unavailable");
});

test("saving a profile preserves reconnect intent and allows an offline endpoint", async ({ page }) => {
  await page.goto("/?mock=ready");
  await page.getByRole("button", { name: "Profiles: RedPill" }).click();
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByLabel("RedPill API key")).toBeEnabled();
  await expect(editor.getByText(/Saving briefly stops protection/)).toHaveCount(0);
  page.once("dialog", (dialog) => dialog.accept());
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(profiles).toBeVisible();
  await profiles.getByRole("button", { name: "New Profile" }).click();
  const candidate = page.getByRole("dialog", { name: "New profile" });
  await expect(candidate.getByText(/Saving briefly stops protection/)).toHaveCount(0);
  await candidate.getByRole("button", { name: "Custom", exact: true }).click();
  await candidate.getByLabel("Service endpoint").fill("https://unreachable.invalid");
  await candidate.getByLabel("API key", { exact: true }).fill("sk-test-candidate");
  await candidate.getByRole("button", { name: "Save" }).click();
  await expect(candidate).toHaveCount(0);
  await expect(profiles.locator(".profile-select")).toHaveCount(2);
  await profiles.getByRole("button", { name: "Done", exact: true }).click();
  await expect(page.getByRole("button", { name: "Profiles: Custom" })).toBeVisible();
});

test("a native proof error remains dismissible without a window close button", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=usage-proof&record=missing");
  await expect(page.getByRole("alert")).toHaveText("Usage record not found");
  await expect(page.getByRole("button", { name: "Done", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByLabel("Usage proof closed")).toBeVisible();
});

test("startup preferences and saved agent links are independent of protection", async ({ page }) => {
  await page.goto("/?mock=ready");
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await nav(page, "Agents").click();
  const codex = page.locator(".agent-block", { hasText: "Codex" });
  await codex.getByRole("switch", { name: "Connect Codex" }).click();
  await expect(codex.getByRole("switch", { name: "Disconnect Codex" })).toBeChecked();
  await page.getByRole("switch", { name: "Start protection" }).click();
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(codex.getByRole("switch", { name: "Disconnect Codex" })).toBeChecked();
  await nav(page, "Settings").click();
  const startup = page.getByRole("region", { name: "General" });
  const connections = page.getByRole("region", { name: "Connections" });
  await expect(connections.getByRole("button", { name: "Profiles", exact: true })).toBeVisible();
  await expect(connections.getByRole("button", { name: "Local API settings", exact: true })).toBeVisible();
  const about = page.getByRole("region", { name: "About" });
  await expect(about.getByRole("button", { name: "Documentation" })).toBeVisible();
  await expect(about.getByRole("button", { name: "GitHub" })).toBeVisible();
  await expect(startup.getByRole("switch", { name: "Open at Login" })).not.toBeChecked();
  await expect(startup.getByRole("switch", { name: "Protect on launch" })).not.toBeChecked();
  await startup.getByRole("switch", { name: "Open at Login" }).click();
  await startup.getByRole("switch", { name: "Protect on launch" }).click();
  await expect(startup.getByRole("switch", { name: "Open at Login" })).toBeChecked();
  await expect(startup.getByRole("switch", { name: "Protect on launch" })).toBeChecked();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();
});

test("Confidential AI presets keep provider credentials scoped and settings stay compact", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();

  const advanced = page.locator(".settings-advanced");
  await expect(advanced.getByRole("button", { name: "Advanced" })).toHaveAttribute("aria-expanded", "false");
  await advanced.getByRole("button", { name: "Advanced" }).click();
  await expect(advanced.getByText("Allow development OS", { exact: true })).toBeVisible();
  const devMode = advanced.getByRole("switch", { name: "Allow development OS" });
  await expect(devMode).toHaveAttribute("aria-checked", "false");
  page.once("dialog", (dialog) => dialog.accept());
  await devMode.click();
  await expect(page.getByText("Dev mode", { exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toHaveClass(/data-checked:bg-warning/);
  page.once("dialog", (dialog) => dialog.accept());
  await devMode.click();

  const localApi = page.locator("section", { has: page.getByRole("heading", { name: "Local API", level: 2 }) });
  await expect(localApi.getByText("Endpoint", { exact: true })).toHaveCount(0);
  await expect(localApi.getByText("Status", { exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: "Profiles", exact: true }).click();
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  const redpill = profiles.locator(".profile-select", { hasText: "RedPill" });
  await expect(redpill).toHaveAttribute("aria-pressed", "true");
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  let editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByRole("button", { name: "RedPill", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(editor.getByLabel("Service endpoint")).toHaveCount(0);

  await editor.getByRole("button", { name: "Phala", exact: true }).click();
  await expect(editor.getByText("Provider", { exact: true })).toBeVisible();
  await expect(editor.getByLabel("Profile name")).toHaveValue("Phala");
  const fieldSpacing = await editor.evaluate((element) => {
    const gap = (labelSelector: string, controlSelector: string) => {
      const label = element.querySelector(labelSelector)?.getBoundingClientRect();
      const control = element.querySelector(controlSelector)?.getBoundingClientRect();
      return label && control ? control.top - label.bottom : null;
    };
    return {
      provider: gap("#profile-provider-label", ".service-presets"),
      name: gap('label[for="profile-name"]', "#profile-name"),
    };
  });
  expect(fieldSpacing.provider).not.toBeNull();
  expect(fieldSpacing.provider).toBe(fieldSpacing.name);
  await expect(editor.getByLabel("Service endpoint")).toHaveCount(0);
  await editor.getByRole("tab", { name: "API key", exact: true }).click();
  await expect(editor.getByLabel("Phala AI API key")).toBeVisible();
  await expect(editor.getByText("Stored securely on this device.")).toBeVisible();
  await expect(editor.getByRole("button", { name: "Save" })).toBeDisabled();

  await editor.getByRole("button", { name: "Custom", exact: true }).click();
  await expect(editor.getByLabel("Profile name")).toHaveValue("Custom");
  await expect(editor.getByLabel("Service endpoint")).toBeEnabled();
  await editor.getByLabel("Service endpoint").fill("https://private.example.com");
  await expect(editor.getByLabel("API key")).toBeVisible();
  await editor.getByRole("button", { name: "Cancel" }).click();

  await profiles.getByRole("button", { name: "New Profile" }).click();
  editor = page.getByRole("dialog", { name: "New profile" });
  await editor.getByRole("button", { name: "Custom", exact: true }).click();
  await expect(editor.getByLabel("Profile name")).toHaveValue("Custom");
  await editor.getByRole("button", { name: "Phala", exact: true }).click();
  await expect(editor.getByLabel("Profile name")).toHaveValue("Phala");
  await editor.getByLabel("Profile name").fill("Research account");
  await editor.getByRole("button", { name: "RedPill", exact: true }).click();
  await expect(editor.getByLabel("Profile name")).toHaveValue("Research account");
  await editor.getByRole("button", { name: "Phala", exact: true }).click();
  await expect(editor.getByRole("button", { name: "Phala", exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(editor.getByRole("button", { name: "Sign in with Phala" })).toBeVisible();
  await editor.getByLabel("Profile name").fill("Private Lab");
  await editor.getByRole("button", { name: "Custom", exact: true }).click();
  await editor.getByLabel("Service endpoint").fill("https://private.example.com");
  await editor.getByLabel("API key").fill("sk-profile-test");
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);
  await expect(profiles).toBeVisible();
  const reopenedProfiles = page.getByRole("dialog", { name: "Profiles" });
  await expect(reopenedProfiles.locator(".profile-select", { hasText: "Private Lab" })).toHaveAttribute("aria-pressed", "true");
  await reopenedProfiles.locator(".profile-select", { hasText: "RedPill" }).click();
  await expect(reopenedProfiles).toHaveCount(0);
  await page.getByRole("button", { name: "Profiles", exact: true }).click();
  const profilesAfterSelection = page.getByRole("dialog", { name: "Profiles" });
  await expect(profilesAfterSelection.locator(".profile-select", { hasText: "RedPill" })).toHaveAttribute("aria-pressed", "true");
  await profilesAfterSelection.getByRole("button", { name: "Edit Private Lab" }).click();
  editor = page.getByRole("dialog", { name: "Edit profile" });
  page.once("dialog", (dialog) => dialog.accept());
  await editor.getByRole("button", { name: "Delete Profile" }).click();
  await expect(editor).toHaveCount(0);
  await expect(profilesAfterSelection.locator(".profile-select", { hasText: "Private Lab" })).toHaveCount(0);

  await profilesAfterSelection.getByRole("button", { name: "Edit RedPill" }).click();
  editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByRole("button", { name: "Delete Profile" })).toBeEnabled();
  page.once("dialog", (dialog) => dialog.accept());
  await editor.getByRole("button", { name: "Delete Profile" }).click();
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);
  await page.getByRole("button", { name: "Profiles", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "New profile" })).toBeVisible();
});

test("native update content renders without a second modal and replays failure state", async ({ page }) => {
  await page.setViewportSize({ width: 480, height: 240 });
  await page.goto("/?mock=ready&native-dialog=update-progress");
  await expect(page.getByRole("heading", { name: "Installing update" })).toBeVisible();
  await expect(page.getByRole("progressbar", { name: "Update progress" })).toBeVisible();
  await expect(page.getByRole("button")).toHaveCount(0);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.goto("/?mock=update-failed&native-dialog=update-progress");
  await expect(page.getByRole("alert")).toContainText("signature");
  await expect(page.getByRole("progressbar")).toHaveCount(0);
  await page.getByRole("button", { name: "Done" }).click();
  await expect(page.getByLabel("Software update closed")).toBeAttached();
  await expect(page.getByRole("heading", { name: "Update failed" })).toHaveCount(0);
});

test("fail-closed states stay explicit and never show the success effects", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=blocked");
  const status = page.getByLabel("Protection status");
  await expect(status.getByText("Protection blocked", { exact: true })).toBeVisible();
  await expect(page.getByText(/identity changed after verification/i)).toBeVisible();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toHaveAttribute("aria-checked", "true");
  await expect(page.locator(".tracks-left, .status-glow")).toHaveCount(0);
  await expect(page.locator(".tracks-right")).toHaveCSS("opacity", "0");
  for (const name of ["Agents", "Usage", "Settings"]) {
    await nav(page, name).click();
    await expect(page.locator(".page-protection").getByText("Protection blocked", { exact: true })).toBeVisible();
    await expect(page.locator(".page-switch-copy")).toHaveClass(/state-danger/);
  }

  await page.goto("/?mock=endpoint-busy");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.getByText(/Address already in use/i)).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeDisabled();
  await nav(page, "Settings").click();
  const general = page.getByRole("region", { name: "General", exact: true });
  await expect(general.locator('[data-slot="separator"]')).toHaveCount(3);
  await expect(general.locator(".row-warning")).toHaveCount(0);
  await expect(page.getByRole("alert")).toContainText("Address already in use");
});

test("responsive, zoomed, dark, high-contrast, and reduced-motion layouts stay bounded and readable", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark", contrast: "more", reducedMotion: "reduce" });
  await page.goto("/?mock=ready");
  for (const viewport of [
    { width: 940, height: 720 },
    { width: 720, height: 600 },
    { width: 540, height: 720 },
    { width: 320, height: 640 },
  ]) {
    await page.setViewportSize(viewport);
    for (const name of ["Overview", "Agents", "Usage", "Settings"]) {
      await nav(page, name).click();
      expect(await overflow(page), `${name} at ${viewport.width}px`).toBeLessThanOrEqual(0);
    }
  }

  await page.setViewportSize({ width: 940, height: 720 });
  await page.evaluate(() => { document.documentElement.style.zoom = "2"; });
  await nav(page, "Overview").click();
  expect(await overflow(page), "Overview at 200% zoom").toBeLessThanOrEqual(0);
  await expect(page.locator(".track-strip").first()).toHaveCSS("animation-name", "none");
  expect(await page.locator(".brand-logo img").first().evaluate((image) => (image as HTMLImageElement).currentSrc)).toContain("brand-mark-dark");

  const audit = await page.evaluate(() => {
    const productText = [...document.querySelectorAll<HTMLElement>("body *")]
      .filter((node) => node.offsetParent !== null && node.childElementCount === 0 && node.textContent?.trim())
      .filter((node) => !node.closest(".track-layer, .sr-only"));
    const tooSmall = productText.filter((node) => Number.parseFloat(getComputedStyle(node).fontSize) < 12);
    const clippedControls = [...document.querySelectorAll<HTMLElement>("button, select, input")]
      .filter((node) => {
        if (node.offsetParent === null) return false;
        const style = getComputedStyle(node);
        const clipsX = node.scrollWidth > node.clientWidth + 1 && style.overflowX !== "hidden";
        const clipsY = node.scrollHeight > node.clientHeight + 1 && style.overflowY !== "hidden";
        return clipsX || clipsY;
      });
    const nestedInteractive = document.querySelectorAll("button button, button input, button select, a button, label button").length;
    return { tooSmall: tooSmall.map((node) => node.textContent), clippedControls: clippedControls.map((node) => node.getAttribute("aria-label") ?? node.textContent), nestedInteractive };
  });
  expect(audit.tooSmall).toEqual([]);
  expect(audit.clippedControls).toEqual([]);
  expect(audit.nestedInteractive).toBe(0);
});

test("Phala completes automatically while RedPill confirms its workspace with Save", async ({ page }) => {
  for (const provider of ["Phala", "RedPill"]) {
    await page.goto("/?mock=no-profiles");
    await page.getByRole("switch", { name: "Start protection" }).click();
    const editor = page.getByRole("dialog", { name: "New profile" });
    await editor.getByRole("button", { name: provider, exact: true }).click();
    const signIn = editor.locator(".sheet-scroll").getByRole("button", { name: `Sign in with ${provider}`, exact: true });
    await expect(signIn.locator("img")).toHaveCount(1);
    await expect(editor.locator(".sheet-footer").getByRole("button", { name: /Sign in|Save/ })).toHaveCount(0);
    await signIn.click();
    if (provider === "RedPill") {
      await expect(editor.getByRole("combobox", { name: "Workspace" })).toContainText("Default");
      await editor.getByRole("button", { name: "Save", exact: true }).click();
    }
    await expect(editor).toHaveCount(0);
    await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();
    await expect(page.getByRole("button", { name: `Profiles: ${provider}` })).toBeVisible();
    if (provider === "Phala") {
      await page.evaluate(() => window.addEventListener("mock:top-up", (event) => {
        if (event instanceof CustomEvent) document.documentElement.dataset.billingScope = event.detail.organizationId;
      }));
      await page.getByRole("button", { name: "Current balance: $12.50", exact: true }).click();
      await expect(page.locator("html")).toHaveAttribute("data-billing-scope", "phala-research");
    }
    await page.getByRole("switch", { name: "Stop protection" }).click();
    await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();
    await page.getByRole("button", { name: `Profiles: ${provider}` }).click();
    const profiles = page.getByRole("dialog", { name: "Profiles", exact: true });
    await expect(profiles.getByText(/Verification required|cannot start protection/)).toHaveCount(0);
    await expect(profiles.getByText(/Ready/)).toBeVisible();
    await profiles.getByRole("button", { name: "Done", exact: true }).click();
    await page.getByRole("switch", { name: "Start protection" }).click();
    await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();
    await expect(page.getByRole("dialog", { name: "Edit profile" })).toHaveCount(0);
  }
});

test("RedPill workspace and organization menu preserve billing scope", async ({ page }) => {
  await page.route("https://img.clerk.com/**", (route) => route.fulfill({ contentType: "image/svg+xml", body: '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><circle cx="12" cy="12" r="12" fill="green"/></svg>' }));
  await page.goto("/?mock=oauth-workspaces");
  await page.evaluate(() => window.addEventListener("mock:top-up", (event) => {
    if (event instanceof CustomEvent) {
      document.documentElement.dataset.topUpProvider = event.detail.provider;
      document.documentElement.dataset.topUpOrganization = event.detail.organizationId ?? "";
    }
  }));
  await page.getByRole("button", { name: "Set up profile" }).click();
  const editor = page.getByRole("dialog", { name: "New profile" });
  await editor.getByRole("button", { name: "RedPill", exact: true }).click();
  await editor.getByRole("button", { name: "Sign in with RedPill" }).click();
  const accountSummary = editor.getByLabel("Account details");
  await expect(accountSummary.getByText("Personal organization", { exact: true })).toBeVisible();
  await expect(accountSummary.getByText("Signed in as Alice Example", { exact: true })).toHaveCount(0);
  await expect(accountSummary.getByRole("img", { name: "Personal organization avatar" })).toBeVisible();
  await expect(accountSummary.getByRole("img", { name: "Alice Example avatar" })).toHaveCount(0);
  await expect(accountSummary.getByText("$12.50", { exact: true })).toBeVisible();
  await expect(accountSummary.getByRole("textbox")).toHaveCount(0);
  await accountSummary.getByRole("button", { name: "Account actions" }).click();
  await expect(page.getByRole("menuitem", { name: "Switch" })).toBeVisible();
  await expect(page.getByRole("menuitem")).toHaveCount(2);
  await expect(page.getByRole("menuitem", { name: "Refresh balance" })).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(page.getByRole("menu")).toHaveCount(0);
  const save = editor.getByRole("button", { name: "Save" });
  await expect(save).toBeDisabled();
  await editor.getByRole("combobox", { name: "Workspace" }).click();
  const research = page.getByRole("option", { name: "Research", exact: true });
  await expect(research).toBeInViewport();
  await expect.poll(() => research.evaluate((option) => {
    const dialog = option.closest("dialog");
    if (!dialog) return false;
    const item = option.getBoundingClientRect();
    const frame = dialog.getBoundingClientRect();
    return dialog.scrollLeft === 0 && item.left >= frame.left && item.right <= frame.right && item.bottom <= frame.bottom;
  })).toBe(true);
  await research.click();
  await expect(save).toBeEnabled();
  await expect(editor.getByText("$12.50", { exact: true })).toBeInViewport();
  await expect(editor.getByRole("button", { name: "Top up", exact: true })).toHaveCount(0);
  await save.click();
  const mainCard = page.getByRole("region", { name: "Protection status" });
  const balanceButton = mainCard.getByRole("button", { name: "Current balance: $12.50", exact: true });
  await expect(balanceButton).toBeVisible();
  await page.evaluate(() => { delete document.documentElement.dataset.topUpProvider; });
  await balanceButton.click();
  await expect(page.locator("html")).toHaveAttribute("data-top-up-provider", "redpill");
  await expect(page.locator("html")).toHaveAttribute("data-top-up-organization", "research-team");
  await expect(mainCard.getByRole("button", { name: "Top up", exact: true })).toHaveCount(0);
  const profileButton = mainCard.getByRole("button", { name: "Profiles: RedPill" });
  const profileBox = await profileButton.boundingBox();
  const balanceBox = await balanceButton.boundingBox();
  expect(profileBox?.y).toBe(balanceBox?.y);
  expect(balanceBox && profileBox && balanceBox.x > profileBox.x).toBe(true);
  await page.getByRole("button", { name: "Profiles: RedPill" }).click();
  await page.getByRole("button", { name: "Edit RedPill" }).click();
  const saved = page.getByRole("dialog", { name: "Edit profile" });
  await expect(saved.getByRole("combobox", { name: "Workspace" })).toContainText("Research");
  await saved.getByRole("tab", { name: "API key", exact: true }).click();
  await expect(saved.getByRole("button", { name: "Save", exact: true })).toBeDisabled();
  await saved.getByLabel("RedPill API key").fill("sk-manual-replacement");
  await expect(saved.getByRole("button", { name: "Save", exact: true })).toBeEnabled();
  await saved.getByRole("tab", { name: "Account", exact: true }).click();
  await expect(saved.getByRole("button", { name: "Save", exact: true })).toBeEnabled();
  await expect(saved.getByText("$12.50", { exact: true })).toBeInViewport();
  await expect(saved.getByRole("button", { name: "Change workspace" })).toHaveCount(0);
  await choose(page, saved.getByRole("combobox", { name: "Workspace" }), "Default");
  await saved.getByRole("button", { name: "Save", exact: true }).click();
  await saved.getByRole("button", { name: "Cancel Sign-in", exact: true }).click();
  await saved.getByRole("button", { name: "Phala", exact: true }).click();
  await expect(saved.getByText("Personal organization", { exact: true })).toHaveCount(0);
  await expect(saved.getByRole("button", { name: "Sign in with Phala" })).toBeVisible();
  await saved.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("button", { name: "Edit RedPill" }).click();
  await expect(saved.getByRole("combobox", { name: "Workspace" })).toContainText("Research");
  await choose(page, saved.getByRole("combobox", { name: "Workspace" }), "Default");
  await saved.getByRole("button", { name: "Save" }).click();
  await page.getByRole("button", { name: "Edit RedPill" }).click();
  await expect(saved.getByRole("combobox", { name: "Workspace" })).toContainText("Default");
  await saved.getByRole("button", { name: "Account actions" }).click();
  await page.getByRole("menuitem", { name: "Switch", exact: true }).click();
  await saved.getByRole("button", { name: "Cancel Sign-in", exact: true }).click();
  await expect(saved.getByRole("combobox", { name: "Workspace" })).toContainText("Default");
});

test("billing permissions hide billing actions but allow scoped organization management", async ({ page }) => {
  await page.addInitScript(() => window.addEventListener("mock:manage-organization", (event) => {
    if (event instanceof CustomEvent) document.documentElement.dataset.managedOrganization = event.detail.organizationId;
  }));
  await page.clock.install();
  for (const mode of ["oauth-balance-denied", "oauth-balance-error", "oauth-balance-readonly"]) {
    await page.goto(`/?mock=${mode}`);
    await page.getByRole("button", { name: "Set up profile" }).click();
    const fresh = page.getByRole("dialog", { name: "New profile" });
    await fresh.getByRole("button", { name: "RedPill", exact: true }).click();
    await fresh.getByRole("button", { name: "Sign in with RedPill" }).click();
    await page.clock.fastForward(3_000);
    await expect(fresh.getByText("Personal organization", { exact: true })).toBeVisible();
    await fresh.getByRole("button", { name: "Save", exact: true }).click();
    await expect(fresh).toHaveCount(0);
    await page.getByRole("button", { name: "Profiles: RedPill" }).click();
    await page.getByRole("button", { name: "Edit RedPill" }).click();
    const editor = page.getByRole("dialog", { name: "Edit profile" });
    await expect(editor.getByText("Personal organization", { exact: true })).toBeVisible();
    await expect(editor.getByRole("button", { name: "Top up", exact: true })).toHaveCount(0);
    await expect(editor.getByText(/\$0\.00/)).toHaveCount(0);
    await expect(editor.getByText(/Unavailable|operation_failed/)).toHaveCount(0);
    await expect(page.getByRole("region", { name: "Protection status" }).getByText("Unavailable", { exact: true })).toHaveCount(0);
    await editor.getByRole("button", { name: "Account actions" }).click();
    await expect(page.getByRole("menuitem", { name: "Switch", exact: true })).toBeVisible();
    await page.getByRole("menuitem", { name: "Manage", exact: true }).click();
    await expect(page.locator("html")).toHaveAttribute("data-managed-organization", "research-team");
    if (mode !== "oauth-balance-readonly") {
      await expect(editor.getByLabel("Balance in USD")).toHaveCount(0);
      await editor.getByRole("button", { name: "Cancel", exact: true }).click();
      await page.getByRole("button", { name: "Done", exact: true }).click();
      await expect(page.getByRole("button", { name: /^Current balance:/ })).toHaveCount(0);
      await page.clock.fastForward(31_000);
      await page.evaluate(() => {
        window.dispatchEvent(new Event("mock:billing-read-granted"));
        window.dispatchEvent(new Event("focus"));
      });
      await expect(page.getByRole("button", { name: "Current balance: $12.50", exact: true })).toBeVisible();
      await page.getByRole("button", { name: "Profiles: RedPill" }).click();
      await page.getByRole("button", { name: "Edit RedPill" }).click();
      await expect(editor.getByLabel("Balance in USD")).toHaveText("$12.50");
      await expect(editor.getByRole("button", { name: "Top up", exact: true })).toHaveCount(0);
    } else {
      await expect(editor.getByLabel("Balance in USD")).toHaveText("$12.50");
    }
  }
});


test("a delayed balance cannot appear after changing provider", async ({ page }) => {
  await page.goto("/?mock=oauth-balance-delayed");
  await page.getByRole("button", { name: "Set up profile" }).click();
  await page.getByRole("button", { name: "Sign in with Phala" }).click();
  await expect(page.getByRole("dialog", { name: "New profile" })).toHaveCount(0);
  await page.getByRole("button", { name: "Profiles: Phala" }).click();
  await page.getByRole("button", { name: "Edit Phala" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByRole("button", { name: "Account actions" })).toBeVisible();
  await expect(editor.getByLabel("Account details")).toBeVisible();
  await editor.getByRole("button", { name: "RedPill", exact: true }).click();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:release-balance")));
  await expect(editor.getByRole("button", { name: "Sign in with RedPill" })).toBeVisible();
  await expect(editor.getByLabel("Account details")).toHaveCount(0);
  await expect(editor.getByText("$12.50", { exact: true })).toHaveCount(0);
});


test("cancelled and declined authorizations leave the form reusable", async ({ page }) => {
  await page.goto("/?mock=oauth-denied");
  await page.getByRole("button", { name: "Set up profile" }).click();
  const editor = page.getByRole("dialog", { name: "New profile" });
  const signIn = editor.getByRole("button", { name: "Sign in with Phala" });
  await signIn.click();
  await editor.getByRole("button", { name: "Cancel Sign-in" }).click();
  await expect(signIn).toBeEnabled();
  await expect(editor.getByRole("button", { name: "Save", exact: true })).toHaveCount(0);
  await signIn.click();
  await expect(editor.getByText("Authorization was declined", { exact: true })).toBeVisible();
  await expect(signIn).toBeEnabled();
  await editor.getByRole("tab", { name: "API key", exact: true }).click();
  await expect(editor.getByLabel("Phala AI API key")).toBeEnabled();
  await editor.getByRole("tab", { name: "Account", exact: true }).click();
  await expect(signIn).toBeEnabled();
  await editor.getByRole("button", { name: "Cancel", exact: true }).click();
  await page.getByRole("button", { name: "Set up profile" }).click();
  await expect(signIn).toBeEnabled();
});

test("account save failure retries the staged grant and saved accounts can be deleted", async ({ page }) => {
  await page.goto("/?mock=oauth-save-retry");
  await page.getByRole("button", { name: "Set up profile" }).click();
  const editor = page.getByRole("dialog", { name: "New profile" });
  await editor.getByRole("button", { name: "RedPill", exact: true }).click();
  await editor.getByRole("button", { name: "Sign in with RedPill" }).click();
  const save = editor.getByRole("button", { name: "Save", exact: true });
  await expect(save).toBeEnabled();
  await save.click();
  await expect(editor.getByText("Could not store account credential", { exact: true })).toBeVisible();
  await expect(editor.getByLabel("Account details").getByText("Personal organization", { exact: true })).toBeVisible();
  await save.click();
  await expect(editor).toHaveCount(0);
  await page.getByRole("button", { name: "Profiles: RedPill" }).click();
  await page.getByRole("button", { name: "Edit RedPill" }).click();
  const saved = page.getByRole("dialog", { name: "Edit profile" });
  await saved.getByLabel("Profile name").fill("Work");
  await saved.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByRole("button", { name: "Edit Work" }).click();
  page.once("dialog", (dialog) => dialog.accept());
  await saved.getByRole("button", { name: "Delete Profile" }).click();
  await expect(saved).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Edit Work" })).toHaveCount(0);
});


test("manual callback fallback keeps RedPill workspace confirmation", async ({ page }) => {
  await page.goto("/?mock=oauth-manual-callback");
  await page.getByRole("button", { name: "Set up profile" }).click();
  const editor = page.getByRole("dialog", { name: "New profile" });
  await editor.getByRole("button", { name: "RedPill", exact: true }).click();
  await editor.getByRole("button", { name: "Sign in with RedPill" }).click();
  await editor.getByText("Paste callback link", { exact: true }).click();
  await editor.getByLabel("Callback URL").fill("https://wrong.example/callback?code=secret");
  await editor.getByRole("button", { name: "Continue", exact: true }).click();
  await expect(editor.getByText("Invalid callback link", { exact: true })).toBeVisible();
  await expect(editor.getByLabel("Callback URL")).toHaveValue("");
  await editor.getByLabel("Callback URL").fill("http://127.0.0.1:4181/oauth/callback?state=mock&code=secret");
  await editor.getByRole("button", { name: "Continue", exact: true }).click();
  await expect(editor.getByRole("combobox", { name: "Workspace" })).toContainText("Default");
  await editor.getByRole("button", { name: "Save", exact: true }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Profiles: RedPill" })).toBeVisible();
});


test("saved organization refreshes its Clerk name and avatar independently of balance", async ({ page }) => {
  await page.route("https://img.clerk.com/**", (route) => route.fulfill({ contentType: "image/svg+xml", body: '<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><circle cx="12" cy="12" r="12" fill="green"/></svg>' }));
  await page.goto("/?mock=oauth-profile-updated");
  await page.getByRole("button", { name: "Set up profile" }).click();
  const fresh = page.getByRole("dialog", { name: "New profile" });
  await fresh.getByRole("button", { name: "RedPill", exact: true }).click();
  await fresh.getByRole("button", { name: "Sign in with RedPill" }).click();
  await expect(fresh.getByText("Personal organization", { exact: true })).toBeVisible();
  await fresh.getByRole("button", { name: "Save", exact: true }).click();
  await page.getByRole("button", { name: "Profiles: RedPill" }).click();
  await page.getByRole("button", { name: "Edit RedPill" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByText("Signed in as Alicia Updated", { exact: true })).toHaveCount(0);
  await expect(editor.getByText("Updated organization", { exact: true })).toBeVisible();
  await expect(editor.getByLabel("Balance in USD")).toHaveText("$12.50");
  await expect(editor.getByRole("img", { name: "Alicia Updated avatar" })).toHaveCount(0);
  await expect(editor.getByRole("img", { name: "Updated organization avatar" })).toHaveAttribute("src", "https://img.clerk.com/updated-org");
});
