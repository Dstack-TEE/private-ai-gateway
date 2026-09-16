import { expect, test } from "@playwright/test";
import { nav } from "./helpers";

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

test("fail-closed states stay explicit and never show the success effects", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=blocked");
  const status = page.getByLabel("Protection status");
  await expect(status.getByText("Protection blocked", { exact: true })).toBeVisible();
  await expect(status.getByRole("alert")).toHaveCount(0);
  await expect(status.locator(".protection-duration")).toHaveCount(0);
  await expect(page.locator(".overview-banner")).toHaveCount(0);
  await expect(page.getByRole("switch", { name: "Stop protection" })).toHaveAttribute("aria-checked", "true");
  for (const name of ["Agents", "Usage", "Settings"]) {
    await nav(page, name).click();
    await expect(page.locator(".page-protection").getByText("Protection blocked", { exact: true })).toBeVisible();
    await expect(page.locator(".page-switch-copy")).toHaveClass(/state-danger/);
  }

  await page.goto("/?mock=endpoint-busy");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.getByText(/Address already in use/i)).toHaveCount(0);
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeDisabled();
  await nav(page, "Settings").click();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("agent operation failures stay out of the active protection status", async ({ page }) => {
  await page.goto("/?mock=agent-write-error");
  const status = page.getByLabel("Protection status");
  await expect(status.getByText("Protected", { exact: true })).toBeVisible();
  await expect(status.locator(".protection-duration")).toBeVisible();

  const failure = page.waitForEvent("dialog");
  const toggle = page.getByRole("switch", { name: "Connect Codex", exact: true }).click();
  const dialog = await failure;
  expect(dialog.message()).toContain("Codex could not connect");
  expect(dialog.message()).toContain("Update Codex before connecting");
  expect(dialog.message()).not.toContain("invalid_state");
  await dialog.dismiss();
  await toggle;
  const agents = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents", exact: true }) });
  await expect(agents.getByRole("alert")).toHaveCount(0);
  await expect(page.getByRole("switch", { name: "Connect Codex", exact: true })).not.toBeChecked();
  await expect(status.getByRole("alert")).toHaveCount(0);
  await expect(status.locator(".protection-duration")).toBeVisible();
  await page.getByRole("switch", { name: "Disconnect Claude Code", exact: true }).click();
  await expect(page.getByRole("switch", { name: "Connect Claude Code", exact: true })).not.toBeChecked();
  await nav(page, "Agents").click();
  await expect(page.getByRole("alert")).toHaveCount(0);
});

test("native shell errors stay on their owning surface", async ({ page }) => {
  await page.goto("/?mock=ready");
  const failure = page.waitForEvent("dialog");
  const action = page.evaluate(() => window.dispatchEvent(new CustomEvent("mock:surface-error", {
    detail: { scope: "agents", message: "Agent menu action failed" },
  })));
  const dialog = await failure;
  expect(dialog.message()).toContain("Agent menu action failed");
  await dialog.dismiss();
  await action;
  const status = page.getByLabel("Protection status");
  const agents = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents", exact: true }) });
  await expect(agents.getByRole("alert")).toHaveCount(0);
  await expect(status.getByRole("alert")).toHaveCount(0);
  await nav(page, "Usage").click();
  await expect(page.getByRole("alert")).toHaveCount(0);
});
