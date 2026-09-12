import { expect, test } from "@playwright/test";
import { modelChartData } from "../src/renderer/components/usage-chart";
import { nav, choose } from "./helpers";

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

test("usage snapshots survive navigation without showing another query's rows", async ({ page }) => {
  await page.goto("/?mock=usage-cache");
  await nav(page, "Usage").click();
  const rows = page.locator(".usage-history tbody tr");
  await expect(rows).toHaveCount(20);
  const first = await rows.first().innerText();
  await page.evaluate(() => { document.documentElement.dataset.holdUsage = "true"; });
  await nav(page, "Overview").click();
  await nav(page, "Usage").click();
  await expect(rows).toHaveCount(20);
  await expect(rows.first()).toHaveText(first, { useInnerText: true });
  await page.evaluate(() => window.dispatchEvent(new Event("mock:refresh-usage")));
  await expect(rows).toHaveCount(20);
  await expect(page.getByText("Loading usage history…", { exact: true })).toHaveCount(0);
  await choose(page, page.getByRole("combobox", { name: "Agent", exact: true }), "Pi");
  await expect(page.getByText("Loading usage history…", { exact: true })).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-usage-query")));
  await expect(page.getByText("Loading usage history…", { exact: true })).toHaveCount(0);
});

test("saved balance remains visible while reopening Overview refreshes it", async ({ page }) => {
  await page.goto("/?mock=oauth-balance-cache");
  await page.getByRole("switch", { name: "Start protection" }).click();
  const editor = page.getByRole("dialog", { name: "New profile" });
  await editor.getByRole("button", { name: "Sign in with Phala" }).click();
  await expect(editor).toHaveCount(0);
  const balance = page.getByRole("button", { name: "Current balance: $12.50", exact: true });
  await expect(balance).toBeVisible();
  await page.evaluate(() => { document.documentElement.dataset.holdBalance = "true"; });
  await nav(page, "Agents").click();
  await nav(page, "Overview").click();
  await expect(balance).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:release-balance")));
});

test("usage history filters, paginates and inspects proof boundaries", async ({ page }) => {
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
