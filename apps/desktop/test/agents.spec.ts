import { expect, test } from "@playwright/test";
import { nav } from "./helpers";

test("visible Agents detects a newly installed OpenCode without reopening the page", async ({ page }) => {
  await page.clock.install();
  await page.goto("/?mock=agent-installed");
  await nav(page, "Agents").click();
  await expect(page.getByRole("region", { name: "Not installed", exact: true })).toContainText("OpenCode");
  await page.evaluate(() => { document.documentElement.dataset.mockAgentInstalled = "true"; });
  await page.clock.fastForward(15_000);
  await expect(page.getByRole("switch", { name: "Connect OpenCode", exact: true })).toBeVisible();
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

test("seven agents connect and disconnect directly from the verified discovered catalog", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=all-agent-icons");
  await nav(page, "Agents").click();

  const rows = page.locator(".agent-block");
  await expect(rows).toHaveCount(7);
  for (const name of ["Codex", "Claude Code", "OpenCode", "Pi", "Hermes Agent", "OpenClaw", "Oh My Pi"]) {
    await expect(rows.filter({ has: page.getByText(name, { exact: true }) })).toBeVisible();
  }
  const agentImageElements = rows.locator(".mark img");
  await expect(agentImageElements).toHaveCount(7);
  await expect.poll(() => agentImageElements.evaluateAll((images) =>
    images.every((image) => image instanceof HTMLImageElement && image.complete && image.naturalWidth > 0),
  )).toBe(true);
  const iconResults = await agentImageElements.evaluateAll((images) =>
    images.map((image) => ({ source: (image as HTMLImageElement).currentSrc })),
  );
  expect(iconResults).toHaveLength(7);
  expect(iconResults.every(({ source }) => source.includes("/assets/") && !source.startsWith("data:"))).toBe(true);

  for (const toggle of await page.getByRole("switch", { name: /^Disconnect / }).all()) {
    await toggle.click();
  }
  for (const name of ["Codex", "Claude Code", "OpenCode", "Pi", "Hermes Agent", "OpenClaw", "Oh My Pi"]) {
    const row = rows.filter({ has: page.getByRole("switch", { name: `Connect ${name}`, exact: true }) });
    await row.getByRole("switch").click();
    await expect(page.getByRole("dialog")).toHaveCount(0);
    const connected = rows.filter({ has: page.getByRole("switch", { name: `Disconnect ${name}`, exact: true }) });
    await expect(connected.getByText("Connected", { exact: true })).toBeVisible();
    await connected.getByRole("switch").click();
    await expect(row.getByText("Not connected", { exact: true })).toBeVisible();
  }

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
