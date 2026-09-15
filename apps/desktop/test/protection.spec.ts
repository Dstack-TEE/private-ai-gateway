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
  await expect(status.getByRole("alert")).toContainText(/identity changed after verification/i);
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
  await expect(page.getByText(/Address already in use/i)).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeDisabled();
  await nav(page, "Settings").click();
  await expect(page.getByRole("alert")).toContainText("Address already in use");
});

test("agent operation failures stay out of the active protection status", async ({ page }) => {
  await page.goto("/?mock=agent-write-error");
  const status = page.getByLabel("Protection status");
  await expect(status.getByText("Protected", { exact: true })).toBeVisible();
  await expect(status.locator(".protection-duration")).toBeVisible();

  await page.getByRole("switch", { name: "Disconnect Claude Code", exact: true }).click();
  const agents = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents", exact: true }) });
  await expect(agents.getByRole("alert")).toContainText("The operation could not complete.");
  await expect(agents.getByRole("alert")).not.toContainText("operation_failed");
  await expect(status.getByRole("alert")).toHaveCount(0);
  await expect(status.locator(".protection-duration")).toBeVisible();
});

test("native shell errors stay on their owning surface", async ({ page }) => {
  await page.goto("/?mock=ready");
  await page.evaluate(() => window.dispatchEvent(new CustomEvent("mock:surface-error", {
    detail: { scope: "agents", message: "Agent menu action failed" },
  })));
  const status = page.getByLabel("Protection status");
  const agents = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents", exact: true }) });
  await expect(agents.getByRole("alert")).toHaveText("Agent menu action failed");
  await expect(status.getByRole("alert")).toHaveCount(0);
  await nav(page, "Usage").click();
  await expect(page.getByRole("alert")).toHaveCount(0);
});
