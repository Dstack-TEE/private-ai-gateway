import { expect, test } from "@playwright/test";
import { nav } from "./helpers";

test("updates are discovered on launch and installation requires confirmation", async ({ page }) => {
  await page.goto("/?mock=update-available");
  const updateBadge = page.getByRole("button", { name: "Update available", exact: true });
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

for (const offline of [false, true]) {
  test(`update installation exposes failure and refreshes its handle (offline=${offline})`, async ({ page }) => {
    await page.goto(`/?mock=update-install-error${offline ? "-offline" : ""}`);
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
    if (offline) {
      await expect(about.getByRole("button", { name: "Install and Restart" })).toHaveCount(0);
      await expect(about).toContainText("Could not check for updates. Retrying automatically.");
    } else {
      await expect(about.getByRole("button", { name: "Install and Restart" })).toBeEnabled();
    }
  });
}

test("settings keep the installed version visible without manual update controls", async ({ page }) => {
  for (const [scenario, message] of [
    ["update-unpublished", "No releases published in this channel yet"],
    ["update-offline", "Could not check for updates. Retrying automatically."],
  ] as const) {
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
  await page.evaluate(() => { window.dispatchEvent(new Event("offline")); window.dispatchEvent(new Event("online")); });
  await expect(status).toHaveText("You're up to date");
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
