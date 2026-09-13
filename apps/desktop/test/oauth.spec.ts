import { expect, test } from "@playwright/test";
import { choose } from "./helpers";

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
  await expect(editor.getByLabel("Workspace", { exact: true })).toHaveCount(0);
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
