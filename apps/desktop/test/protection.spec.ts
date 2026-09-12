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
  await expect(page.getByText(/identity changed after verification/i)).toBeVisible();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toHaveAttribute("aria-checked", "true");
  await expect(page.locator(".tracks-left, .status-glow")).toHaveCount(0);
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
