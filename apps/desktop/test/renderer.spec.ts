import { expect, test } from "@playwright/test";
import { modelChartData } from "../src/renderer/components/usage-chart";
import { usageDateBounds } from "../src/renderer/lib/usage-dates";
import { localApiExample } from "../src/renderer/lib/local-api-example";
import { createServer } from "node:http";
import { execFile } from "node:child_process";
import { promisify } from "node:util";

type Page = import("@playwright/test").Page;

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
      const executable = language === "curl" ? "/bin/sh" : language === "python" ? "python3" : process.execPath;
      const args = language === "javascript" ? ["--input-type=module", "-e", code] : ["-c", code];
      await run(executable, args, { env: { ...process.env, PAG_API_KEY: "wrong-key" }, timeout: 5_000 });
    }
    expect(calls).toHaveLength(3);
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
  await expect(dialog.locator("code")).toContainText("http://127.0.0.1:4180/v1/chat/completions");
  await expect(dialog.locator("code")).toContainText("sk-pag-");
  await dialog.getByRole("combobox", { name: "Model", exact: true }).selectOption("zai/glm-5.2");
  for (const language of ["cURL", "Python", "JavaScript"]) {
    await dialog.getByRole("tab", { name: language, exact: true }).click();
    await expect(dialog.locator("code")).toContainText("zai/glm-5.2");
    await expect(dialog.locator("code")).toContainText("sk-pag-");
    await dialog.getByRole("button", { name: "Copy example", exact: true }).click();
    await expect(dialog.getByRole("status")).toHaveText("Example copied");
  }
  await dialog.getByRole("button", { name: "Done", exact: true }).click();
  await expect(dialog).toHaveCount(0);
});

const nav = (page: Page, name: string) =>
  page.getByRole("navigation", { name: "Main navigation" }).getByRole("button", { name });

test("appearance defaults to system, persists and settings shortcut navigates", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark" });
  await page.goto("/?mock=ready");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await page.keyboard.press("Meta+,");
  await expect(page.getByRole("heading", { name: "Settings", exact: true })).toBeVisible();
  const theme = page.getByRole("combobox", { name: "Theme", exact: true });
  await expect(theme).toHaveValue("system");
  await theme.selectOption("light");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await expect(page.locator("html")).toHaveCSS("color-scheme", "light");
  await page.reload();
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
  await page.keyboard.press("Control+,");
  await theme.selectOption("dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await theme.selectOption("system");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "light");
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

test("custom date ranges apply atomically to chart, table and export", async ({ page }) => {
  await page.clock.setFixedTime(new Date("2026-09-06T12:00:00"));
  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();
  const dateButton = page.getByRole("button", { name: /^Date range:/ });
  await dateButton.click();
  await page.locator('[data-day="9/2/2026"]').click();
  await page.getByRole("dialog", { name: "Choose date range" }).getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(dateButton).toHaveAccessibleName("Date range: Last 7 days");
  await dateButton.click();
  await page.getByRole("combobox", { name: "Quick date range" }).selectOption("all");
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
  await page.getByRole("button", { name: "Export usage as CSV" }).click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText(`Exported ${timestamps.length} usage records`);
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

  await page.getByRole("button", { name: "Private AI Gateway menu" }).click();
  const tray = page.getByRole("menu", { name: "Private AI Gateway" });
  await expect(tray.getByRole("switch")).toHaveCount(0);
  await expect(tray.getByRole("menuitem", { name: "Stop protection" })).toBeVisible();
  for (const name of ["Open Private AI Gateway", "Settings…", "Quit Private AI Gateway"]) {
    await expect(tray.getByRole("menuitem", { name })).toBeVisible();
  }
  const openAtLogin = tray.getByRole("menuitemcheckbox", { name: "Open at Login" });
  await expect(openAtLogin).toHaveAttribute("aria-checked", "false");
  await openAtLogin.click();
  await expect(openAtLogin).toHaveAttribute("aria-checked", "true");

  const brandImageElements = page.locator(".brand-logo img");
  await expect(brandImageElements).toHaveCount(3);
  await expect.poll(() => brandImageElements.evaluateAll((images) =>
    images.every((image) => image instanceof HTMLImageElement && image.complete && image.naturalWidth > 0),
  )).toBe(true);
  const brandIcons = await brandImageElements.evaluateAll((images) =>
    images.map((image) => ({ source: (image as HTMLImageElement).currentSrc })),
  );
  expect(brandIcons).toHaveLength(3);
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
  await expect(settingsDialog).toBeFocused();
  await expect(settingsDialog).toHaveCSS("outline-style", "none");

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
  await editor.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByRole("dialog", { name: "Profiles" })).toBeVisible();
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
      text: "sk-pag-",
    },
    {
      size: { width: 580, height: 510 },
      path: "/?mock=ready&native-dialog=profile-editor",
      name: "New profile",
      text: "Verify and Save",
    },
    {
      size: { width: 700, height: 680 },
      path: "/?mock=ready&native-dialog=privacy",
      name: "Privacy verification",
      text: "Attested encrypted channel",
    },
    {
      size: { width: 600, height: 680 },
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
  ] as const;

  for (const entry of cases) {
    await page.setViewportSize(entry.size);
    await page.goto(entry.path);
    await expect(page.locator(".desktop-window, .sidebar")).toHaveCount(0);
    const dialog = page.getByRole("dialog", { name: entry.name });
    await expect(dialog).toContainText(entry.text);
    await expect(page.locator("html")).not.toHaveAttribute("data-loading-frame-observed", "true");
    expect(await dialog.boundingBox()).toEqual({ x: 0, y: 0, ...entry.size });
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

test("usage query failures do not display stale totals and clearing preserves the selected filter", async ({ page }) => {
  await page.goto("/?mock=usage-query-error");
  await nav(page, "Usage").click();
  const history = page.getByRole("table", { name: "Usage history", exact: true });
  await expect(history.getByRole("button").first()).toBeVisible();
  const model = page.getByRole("combobox", { name: "Model", exact: true });
  const selected = await model.locator("option").nth(1).getAttribute("value");
  expect(selected).toBeTruthy();
  await model.selectOption(selected ?? "");
  await expect(page.getByRole("alert")).toHaveText("Usage database temporarily unavailable");
  await expect(history.getByRole("button")).toHaveCount(0);
  await expect(page.locator(".usage-stats strong")).toHaveText(["—", "—", "—", "—"]);
  await expect(model).toHaveValue(selected ?? "");
  await expect(page.getByText("Usage data unavailable.")).toBeVisible();

  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();
  await model.selectOption(selected ?? "");
  await expect(history.getByRole("button").first()).toBeVisible();
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Clear usage history" }).click();
  await expect(history.getByRole("button")).toHaveCount(0);
  await expect(model).toHaveValue(selected ?? "");
  await expect(page.locator(".usage-stats strong").first()).toHaveText("0");
});

test("rotating the client key requires an explicit native confirmation", async ({ page }) => {
  await page.goto("/?mock=interactive&native-dialog=local-api");
  const key = page.getByLabel("Client key", { exact: true });
  await expect(key).not.toHaveValue("");
  await expect(page.locator(".sheet-card")).toHaveCount(0);
  await expect(key).toHaveCSS("height", "36px");
  await expect(key).toHaveCSS("border-radius", "24px");
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
    await expect(key).toHaveValue(/^sk-pag-/);
    await expect(page.getByRole("button", { name: "Copy client key" })).toBeEnabled();
    await expect(page.getByRole("alert")).toHaveCount(0);
  }
});

test("protection flow, page headers, and focus follow the native desktop contract", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=no-profiles");

  await expect(page).toHaveTitle("Private AI Gateway");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.locator(".gateway-verdict")).toHaveCSS("color", await themeColor(page, "--muted-foreground"));
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);
  let editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toBeVisible();
  await expect(editor.getByRole("button", { name: "Phala" })).toHaveAttribute("aria-pressed", "true");
  await editor.getByRole("button", { name: "Cancel" }).click();
  await page.getByRole("switch", { name: "Start protection" }).click();
  editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toBeVisible();
  await editor.getByLabel("Phala AI API key").fill("sk-test-123");
  await editor.getByRole("button", { name: "Verify and Save" }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);

  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();

  await nav(page, "Overview").focus();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Agents")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Usage")).toBeFocused();

  await nav(page, "Settings").click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();
  await expect(page.getByRole("button", { name: "Profiles", exact: true })).toContainText("Ready");
  await page.getByRole("switch", { name: "Start protection" }).click();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();

  await nav(page, "Overview").click();
  await expect(page.getByRole("heading", { name: "Overview", level: 1 })).toBeFocused();
  await expect(page.getByLabel("Protection status").getByText("Protected", { exact: true })).toBeVisible();
  await expect(page.locator(".tracks-left")).toHaveCSS("opacity", "0.07");
  await expect(page.locator(".tracks-right")).toHaveCSS("opacity", "0.12");

  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.locator(".tracks-left")).toHaveCSS("opacity", "0");
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
  page.once("dialog", async (dialog) => {
    expect(dialog.type()).toBe("confirm");
    expect(dialog.message()).toContain("Restore all agents?");
    await dialog.accept();
  });
  await page.getByRole("button", { name: "Restore all" }).click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText("All agent configurations restored");
});

test("overview shows four agents, four current-session records, truthful copy surfaces, and session totals", async ({ page }) => {
  await page.setViewportSize({ width: 1100, height: 900 });
  await page.goto("/?mock=ready");

  const agentsModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents" }) });
  await expect(agentsModule.locator(".agent-block")).toHaveCount(4);
  await expect(agentsModule.locator(".agent-block").last()).toBeVisible();
  const usageModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Recent usage" }) });
  await expect(usageModule.locator(".usage-row")).toHaveCount(4);
  await expect(usageModule.locator(".usage-row").last()).toBeVisible();
  expect(await agentsModule.locator(".module").evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true);
  expect(await usageModule.locator(".module").evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true);
  expect(await page.locator(".content").evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
  const spacing = await page.locator(".overview-page").evaluate((node) => {
    const modules = Array.from(node.querySelectorAll(".overview-module"), (item) => item.getBoundingClientRect());
    const surface = node.querySelector(".status-surface")?.getBoundingClientRect();
    if (!surface || modules.length !== 4) throw new Error("Overview modules missing");
    return [modules[0].top - surface.bottom, modules[2].top - modules[0].bottom];
  });
  expect(Math.abs(spacing[0] - spacing[1])).toBeLessThanOrEqual(1);
  await expect(page.locator(".overview-module-title").first()).toHaveCSS("user-select", "none");
  await usageModule.locator(".usage-row").first().click();
  const overviewProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(overviewProof).toContainText("Signed receipt verified");
  await overviewProof.getByRole("button", { name: "Done" }).click();

  const session = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Usage in this session" }) });
  const localApi = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Local API" }) });
  for (const label of ["Requests", "Tokens", "Cost", "Protected"]) {
    await expect(session.getByText(label, { exact: true })).toBeVisible();
  }
  await expect(session.locator("small")).toHaveCount(0);
  await expect(session.getByText("This session", { exact: true })).toHaveCount(0);
  await expect(localApi.locator(".overview-module-title").getByText("Available", { exact: true })).toBeVisible();
  await expect(localApi.locator(".copy-rows").getByText("Available", { exact: true })).toHaveCount(0);
  await expect(localApi.getByText("for your own tools", { exact: true })).toHaveCount(0);
  for (const module of [localApi, session]) {
    const height = await module.locator(".module").evaluate((node) => node.getBoundingClientRect().height);
    expect(height).toBe(136);
  }

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
  await expect(clientKey).toContainText("sk-pag-");

  await localApi.getByRole("button", { name: "Local API settings" }).click();
  const localSheet = page.getByRole("dialog", { name: "Local API settings" });
  await expect(localSheet).toBeVisible();
  for (const label of ["Listen address", "Allow network access", "Port", "Client host", "Client key"]) {
    await expect(localSheet.getByText(label, { exact: true }).first()).toBeVisible();
  }
  await expect(localSheet.getByRole("button", { name: /Copy .*endpoint/ })).toHaveCount(0);
  const networkToggle = localSheet.getByRole("switch", { name: "Allow network access" });
  await expect(networkToggle.locator('xpath=ancestor::*[@data-slot="item"][1]')).toHaveAttribute("data-variant", "outline");
  await expect(localSheet.getByRole("group", { name: "Client endpoints", exact: true })).toHaveCount(0);
  await expect(localSheet.locator('[data-slot="field-separator"]')).toHaveCount(1);
  await expect(localSheet.locator('.sheet-footer > [data-slot="separator"]')).toHaveCSS("height", "1px");
  await networkToggle.click();
  await expect(localSheet.getByRole("alert")).toContainText("trusted network");
  await networkToggle.click();
  await expect(localSheet.getByText("Access keys", { exact: true })).toHaveCount(0);
  await expect(localSheet.getByRole("button", { name: "Save" })).toBeEnabled();
  await expect(localSheet).toContainText("Saving briefly restarts protection");
  await localSheet.getByLabel("Port", { exact: true }).fill("4181");
  await localSheet.getByRole("button", { name: "Save", exact: true }).click();
  await expect(localSheet).not.toBeVisible();
  await expect(endpoint).toContainText("4181");
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(session.locator("strong")).toHaveText(["—", "—", "—", "—"]);
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
  const channel = page.locator(".settings-advanced").getByRole("combobox", { name: "Update channel" });
  await expect(channel).toHaveValue("stable");
  await channel.selectOption("beta");
  await expect(page.getByRole("status").filter({ hasText: "Version 0.3.0-beta.1 is available" })).toBeVisible();
  await nav(page, "Overview").click();
  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Advanced", exact: true }).click();
  await expect(channel).toHaveValue("beta");
  await channel.selectOption("stable");
  await expect(page.getByRole("status").filter({ hasText: "Version 0.2.0 is available" })).toBeVisible();
});

test("success colors, list separators, control sizes and About alignment are consistent", async ({ page }) => {
  await page.setViewportSize({ width: 1052, height: 784 });
  for (const colorScheme of ["light", "dark"] as const) {
    await page.emulateMedia({ colorScheme });
    await page.goto("/?mock=ready");
    const success = await themeColor(page, "--success");
    expect(success).not.toBe(await themeColor(page, "--primary"));
    const local = page.locator(".overview-module-title", { has: page.getByRole("heading", { name: "Local API", exact: true }) });
    await expect(local.locator('[data-slot="badge"]').filter({ hasText: /^Available$/ })).toHaveCSS("color", success);
    await expect(page.locator(".status-local .status-fact").filter({ hasText: "1 agent connected" })).toHaveCSS("color", success);
    await expect(page.getByLabel("Protection status").getByRole("switch")).toHaveCSS("width", "60px");
    await expect(page.getByLabel("Protection status").getByRole("switch")).toHaveCSS("height", "28px");
    await expect(page.getByLabel("Protection status").getByRole("switch")).toHaveCSS("background-color", success);
    await expect(page.locator(".tracks-right")).toHaveCSS("color", await themeColor(page, "--primary"));
    await expect(page.getByRole("button", { name: "Profiles: RedPill" })).toHaveCSS("width", "128px");
    await expect(nav(page, "Agents")).toHaveCSS("height", "36px");
    await nav(page, "Agents").click();
    const installed = page.getByRole("region", { name: /^Installed/ });
    const separators = installed.locator('[data-slot="separator"]:visible');
    expect(await separators.count()).toBe((await installed.locator(".agent-block").count()) - 1);
    await expect(installed.locator('[data-slot="badge"]', { hasText: /^Connected$/ }).first()).toHaveCSS("color", success);
    await expect(page.locator(".page-header").getByRole("switch")).toHaveCSS("width", "44px");
    await expect(page.locator(".page-header").getByRole("switch")).toHaveCSS("background-color", success);
    await nav(page, "Usage").click();
    await expect(page.locator(".page-header").getByRole("switch")).toHaveCSS("background-color", success);
    await nav(page, "Settings").click();
    await expect(page.locator(".page-header").getByRole("switch")).toHaveCSS("background-color", success);
    const general = page.getByRole("region", { name: "General", exact: true });
    await expect(general.locator('[data-slot="separator"]')).toHaveCount(4);
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
    await expect.poll(() => badge.evaluate((node) => {
      const dot = node.querySelector('[data-slot="status-dot"]');
      return dot !== null && getComputedStyle(dot).backgroundColor === getComputedStyle(node).color;
    })).toBe(true);
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
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Update available", exact: true }).click();
  const progress = page.getByRole("dialog", { name: "Installing update", exact: true });
  await expect(progress).toBeVisible();
  await expect(progress.getByRole("progressbar", { name: "Update progress" })).toHaveAttribute("aria-valuenow", "40");
  await page.keyboard.press("Escape");
  await expect(progress).toBeVisible();
  await expect(progress.getByRole("button", { name: "Cancel" })).toHaveCount(0);
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-update")));
  const failure = page.getByRole("dialog", { name: "Update failed", exact: true });
  await expect(failure).toBeVisible();
  await failure.getByRole("button", { name: "Done" }).click();
  await expect(failure).toHaveCount(0);
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
  await page.getByRole("table", { name: "Usage history", exact: true }).getByRole("button", { name: "Token details", exact: true }).first().click();
  await expect(page.getByRole("dialog", { name: "Token details", exact: true })).toBeVisible();
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
    await expect(advanced.getByRole("combobox", { name: "Update channel" })).toBeVisible();
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
    const group = button.closest(".module");
    if (!row || !group) throw new Error("Copy row structure missing");
    return { height: button.getBoundingClientRect().height, rowHeight: row.getBoundingClientRect().height, clipped: getComputedStyle(group).overflow, radius: getComputedStyle(group).borderRadius };
  });
  expect(Math.abs(shape.height - shape.rowHeight)).toBeLessThanOrEqual(1);
  expect(shape.clipped).toBe("hidden");
  expect(Number.parseFloat(shape.radius)).toBeGreaterThan(0);
  const profile = page.getByRole("button", { name: "Profiles: RedPill" });
  await expect(profile).toHaveAttribute("aria-haspopup", "dialog");
  await profile.click();
  await expect(page.getByRole("dialog", { name: "Profiles", exact: true })).toBeVisible();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await expect(profile).toBeFocused();
});

test("usage history filters, paginates, inspects proof boundaries, exports, and clears explicitly", async ({ page }) => {
  await page.clock.setFixedTime(new Date("2026-09-06T12:00:00"));
  await page.setViewportSize({ width: 940, height: 760 });
  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();

  await expect(page.locator('[data-slot="chart"] .recharts-surface')).toBeVisible();
  await expect(page.getByRole("table", { name: "Usage by model", includeHidden: true }).locator("tbody tr")).toHaveCount(7);
  const chartDays = await page.getByRole("table", { name: "Usage by model", includeHidden: true }).locator("tbody th").allTextContents(
  );
  expect(new Set(chartDays).size).toBe(7);

  await page.getByRole("button", { name: /^Date range:/ }).click();
  await page.getByRole("combobox", { name: "Quick date range" }).selectOption("24h");
  await expect(page.getByRole("table", { name: "Usage by model", includeHidden: true }).locator("tbody tr")).toHaveCount(1);
  await expect.poll(async () => page.locator('.usage-history time').evaluateAll((times) => times.length > 0 && times.every((time) => {
    const midnight = new Date();
    midnight.setHours(0, 0, 0, 0);
    return new Date(time.getAttribute("datetime") ?? "").getTime() >= midnight.getTime();
  }))).toBe(true);
  await page.getByRole("button", { name: /^Date range:/ }).click();
  await page.getByRole("combobox", { name: "Quick date range" }).selectOption("7d");

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
  await history.getByRole("button", { name: "Token details", exact: true }).first().click();
  await expect(page.getByRole("dialog", { name: "Token details", exact: true })).toContainText("Cache read");
  await page.keyboard.press("Escape");
  await page.getByRole("combobox", { name: "Rows per page" }).selectOption("50");
  await expect(history.locator("tbody tr")).not.toHaveCount(20);
  await expect(page.getByRole("button", { name: "Next usage page" })).toBeDisabled();
  await page.getByRole("combobox", { name: "Rows per page" }).selectOption("20");
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
  await agentFilter.selectOption("hermes");
  await expect(history.getByRole("button").first()).toContainText("Hermes");
  await agentFilter.selectOption("");
  const blocked = history.getByRole("button", { name: /Blocked locally/ }).first();
  await blocked.click();
  const blockedProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(blockedProof.getByText("Blocked locally", { exact: true })).toBeVisible();
  await expect(blockedProof.getByText(/did not leave this Mac/)).toBeVisible();
  await expect(blockedProof.getByText("Request kept on this Mac", { exact: true })).toBeVisible();
  await expect(blockedProof.locator(".proof-flow, .privacy-verdict.state-success")).toHaveCount(0);
  await blockedProof.getByRole("button", { name: "Done" }).click();
  await expect(history).not.toContainText("/v1/models");

  await page.getByRole("button", { name: "Export usage as CSV" }).click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText(/Exported \d+ usage records/);

  page.once("dialog", async (dialog) => {
    expect(dialog.message()).toContain("Clear usage history?");
    await dialog.dismiss();
  });
  await page.getByRole("button", { name: "Clear usage history" }).click();
  await expect(history.locator("tbody tr")).toHaveCount(20);

  page.once("dialog", async (dialog) => {
    expect(dialog.type()).toBe("confirm");
    expect(dialog.message()).toContain("Clear usage history?");
    await dialog.accept();
  });
  await page.getByRole("button", { name: "Clear usage history" }).click();
  await expect(page.locator(".usage-history")).toContainText("No saved usage matches these filters.");
  await expect(page.locator('.sr-only[role="status"]')).toContainText(/Deleted \d+ usage records/);
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
  await expect(status.getByText("Local API available", { exact: true })).toBeVisible();
  await expect(status.getByText("1 agent connected", { exact: true })).toBeVisible();
  await expect(status.getByText("Confidential AI", { exact: true })).toBeVisible();
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
    const facts = Array.from(node.querySelectorAll(".status-local .status-fact"));
    const text = facts.map((fact) => fact.lastElementChild?.getBoundingClientRect().x);
    const icons = facts.map((fact) => fact.firstElementChild?.getBoundingClientRect().width);
    const left = node.querySelector(".status-local .status-heading")?.getBoundingClientRect();
    const right = node.querySelector(".status-remote .status-heading")?.getBoundingClientRect();
    const verified = node.querySelector('[aria-label="Privacy verification"]')?.getBoundingClientRect();
    const profile = node.querySelector(".status-profile")?.getBoundingClientRect();
    return { text, icons, heights: facts.map((fact) => fact.getBoundingClientRect().height), headings: [left?.y, right?.y], verifiedHeight: verified?.height, buttonRightEdges: [verified?.right, profile?.right] };
  });
  expect(alignment.text[0]).toBe(alignment.text[1]);
  expect(alignment.icons).toEqual([14, 14]);
  expect(alignment.heights).toEqual([18, 18]);
  expect(alignment.headings[0]).toBe(alignment.headings[1]);
  expect(alignment.verifiedHeight).toBe(32);
  expect(alignment.buttonRightEdges[0]).toBe(alignment.buttonRightEdges[1]);
  await status.getByRole("button", { name: "Privacy verification", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Privacy verification" })).toBeVisible();
  await page.getByRole("dialog", { name: "Privacy verification" }).getByRole("button", { name: "Done", exact: true }).click();
  await expect(status.getByText(/answers this session/i)).toHaveCount(0);

  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(status.getByRole("button", { name: "Privacy verification", exact: true })).toHaveCount(0);
  await expect(status.getByText("Not connected", { exact: true })).toBeVisible();
  await expect(page.locator(".tray-template-icon")).toHaveCSS("opacity", "0.45");
  for (const name of ["Agents", "Usage", "Settings"]) {
    await nav(page, name).click();
    await expect(page.locator(".page-switch-copy")).toHaveCSS("color", await themeColor(page, "--muted-foreground"));
  }
  await expect(page.getByRole("button", { name: "Profiles", exact: true })).toHaveCSS("border-bottom-width", "0px");
  await nav(page, "Agents").click();
  await expect(page.getByRole("button", { name: "Detect installed agents" })).toHaveCount(0);

  await page.goto("/?mock=no-key");
  await expect(page.getByLabel("Protection status").getByText("Credential unavailable", { exact: true })).toBeVisible();
  await page.getByRole("switch", { name: "Start protection" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor).toBeVisible();
  await expect(editor.getByLabel("RedPill API key")).toBeVisible();
});

test("installed agents stay ordered and protection state is consistent across pages", async ({ page }) => {
  await page.goto("/?mock=mixed-agents");
  await expect(page.locator(".status-agent-icon")).toHaveCount(4);
  await expect(page.locator(".status-agent-icon.is-disconnected")).toHaveCount(3);
  const preview = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents", exact: true }) });
  await expect(preview.locator(".agent-block")).toHaveCount(4);
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
  await expect(page.getByText("Local API unavailable", { exact: true })).toBeVisible();
});

test("editing a live profile reconnects, while a failed candidate stays unsaved and unprotected", async ({ page }) => {
  await page.goto("/?mock=ready");
  await page.getByRole("button", { name: "Profiles: RedPill" }).click();
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByLabel("RedPill API key")).toBeEnabled();
  await expect(editor.getByText(/Saving briefly stops protection/)).toBeVisible();
  await editor.getByRole("button", { name: "Verify and Save" }).click();
  await expect(profiles).toBeVisible();
  await profiles.getByRole("button", { name: "New Profile" }).click();
  const candidate = page.getByRole("dialog", { name: "New profile" });
  await expect(candidate.getByText(/Saving briefly stops protection/)).toBeVisible();
  await candidate.getByRole("button", { name: "Custom", exact: true }).click();
  await candidate.getByLabel("Service endpoint").fill("https://unreachable.invalid");
  await candidate.getByLabel("API key", { exact: true }).fill("sk-test-candidate");
  await candidate.getByRole("button", { name: "Verify and Save" }).click();
  await expect(candidate.getByRole("alert")).toContainText("did not answer");
  await candidate.getByRole("button", { name: "Cancel" }).click();
  await expect(profiles.locator(".profile-select")).toHaveCount(1);
  await profiles.getByRole("button", { name: "Done", exact: true }).click();
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
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
  await expect(startup.getByRole("button", { name: "Profiles", exact: true })).toBeVisible();
  await expect(startup.getByRole("button", { name: "Local API settings", exact: true })).toBeVisible();
  const about = page.getByRole("region", { name: "About" });
  await expect(about.getByRole("button", { name: "Documentation" })).toBeVisible();
  await expect(about.getByRole("button", { name: "GitHub" })).toBeVisible();
  await expect(startup.getByRole("switch", { name: "Open at Login" })).not.toBeChecked();
  await expect(startup.getByRole("switch", { name: "Connect on launch" })).not.toBeChecked();
  await startup.getByRole("switch", { name: "Open at Login" }).click();
  await startup.getByRole("switch", { name: "Connect on launch" }).click();
  await expect(startup.getByRole("switch", { name: "Open at Login" })).toBeChecked();
  await expect(startup.getByRole("switch", { name: "Connect on launch" })).toBeChecked();
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
  await devMode.click();
  await expect(page.getByText("Dev mode", { exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toHaveClass(/is-development/);
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
  await expect(editor.getByRole("button", { name: "RedPill" })).toHaveAttribute("aria-pressed", "true");
  await expect(editor.getByLabel("Service endpoint")).toHaveValue("https://tee.redpill.ai");
  await expect(editor.getByLabel("Service endpoint")).toBeDisabled();

  await editor.getByRole("button", { name: "Phala" }).click();
  await expect(editor.getByLabel("Service endpoint")).toHaveValue("https://inference.phala.com");
  await expect(editor.getByLabel("Phala AI API key")).toBeVisible();
  await expect(editor.getByText("A key is required for a new provider or endpoint.")).toBeVisible();
  await expect(editor.getByRole("button", { name: "Verify and Save" })).toBeDisabled();

  await editor.getByRole("button", { name: "Custom" }).click();
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
  await expect(editor.getByRole("button", { name: "Phala" })).toHaveAttribute("aria-pressed", "true");
  await expect(editor.getByRole("button", { name: "Verify and Save" })).toBeVisible();
  await editor.getByLabel("Profile name").fill("Private Lab");
  await editor.getByRole("button", { name: "Custom" }).click();
  await editor.getByLabel("Service endpoint").fill("https://private.example.com");
  await editor.getByLabel("API key").fill("sk-profile-test");
  await editor.getByRole("button", { name: "Verify and Save" }).click();
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
  await expect(page.locator(".tracks-left")).toHaveCSS("opacity", "0");
  await expect(page.locator(".status-glow")).toHaveCSS("opacity", "0");
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
  await expect(general.locator('[data-slot="separator"]')).toHaveCount(4);
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
