import { expect, test } from "@playwright/test";
import { nav } from "./helpers";

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
