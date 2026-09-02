import { expect, test } from "@playwright/test";

type Page = import("@playwright/test").Page;
// Horizontal overflow of the document or of the scrolling content area.
const overflow = (page: Page) =>
  page.evaluate(() => {
    const view = document.querySelector(".content");
    return Math.max(
      document.documentElement.scrollWidth - document.documentElement.clientWidth,
      view ? view.scrollWidth - view.clientWidth : 0,
    );
  });
const tab = (page: Page, name: string) => page.getByRole("tab", { name });

test("cold start to protected, connect an agent, recover, and shut down", async ({ page }) => {
  await page.goto("/?mock=interactive");
  const status = page.getByLabel("Protection status");
  await expect(page).toHaveTitle("Private AI Gateway");
  await expect(status.getByText("Not protected")).toBeVisible();

  // The segmented control is a tab list: arrow keys move between views.
  await tab(page, "Overview").focus();
  await page.keyboard.press("ArrowRight");
  await expect(tab(page, "Activity")).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("ArrowRight");
  await expect(tab(page, "Settings")).toHaveAttribute("aria-selected", "true");

  // The key lives in Settings; the overview stays a verdict and a switch.
  await expect(page.getByText("Not saved.")).toBeVisible();
  await page.getByLabel("RedPill API key").fill("sk-test-123");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("Saved in the system credential store")).toBeVisible();

  // A failed verification is recovered from Settings; the switch cancels a
  // verification in progress.
  await page.getByLabel("AI service").fill("https://unreachable.invalid");
  await tab(page, "Overview").click();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("button", { name: "Start", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(status.getByText("Verification failed")).toBeVisible();
  await expect(status.getByText("did not answer the model list request")).toBeVisible();
  await page.getByRole("button", { name: "Open Settings" }).click();
  await page.getByLabel("AI service").fill("https://tee.redpill.ai");
  await tab(page, "Overview").click();
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(status.getByText("Protected", { exact: true })).toBeVisible();

  // Connect Claude Code: the model is chosen inside the sheet, the summary
  // follows it, the config diff sits behind a disclosure, and focus stays
  // contained until Connect.
  const claudeRow = page.locator(".agent-block", { hasText: "Claude Code" });
  await claudeRow.getByRole("button", { name: "Connect", exact: true }).click();
  const sheet = page.getByRole("dialog");
  await expect(sheet.getByRole("button", { name: "Connect", exact: true })).toBeDisabled();
  await sheet.getByLabel("Model for Claude Code").selectOption("openai/gpt-oss-20b");
  await expect(sheet.getByText("using openai/gpt-oss-20b")).toBeVisible();
  await sheet.getByText(/Configuration changes \(\d+\)/).click();
  await expect(sheet.getByText("Existing secret")).toBeVisible();
  for (let i = 0; i < 4; i += 1) {
    await page.keyboard.press("Tab");
    const escaped = await page.evaluate(() => {
      const active = document.activeElement;
      return active !== null && active !== document.body && active.closest("dialog") === null;
    });
    expect(escaped).toBe(false);
  }
  await sheet.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(claudeRow.getByText("Connected", { exact: true })).toBeVisible();

  // Escape closes the disconnect sheet and returns focus to the trigger.
  const disconnect = claudeRow.getByRole("button", { name: "Disconnect", exact: true });
  await disconnect.click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toBeHidden();
  await expect(disconnect).toBeFocused();

  // Restore all lives in Settings and disconnects every recorded agent.
  await tab(page, "Settings").click();
  await page.getByRole("button", { name: "Restore All…" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Restore All" }).click();
  await tab(page, "Overview").click();
  await expect(claudeRow.getByText("Not connected", { exact: true })).toBeVisible();

  // Deleting the key keeps verification but stops protection; Stop ends it.
  await tab(page, "Settings").click();
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await tab(page, "Overview").click();
  await expect(status.getByText("API key needed")).toBeVisible();
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect(status.getByText("Not protected")).toBeVisible();
});

test("activity is a list with an inspector, and settings show the proofs and the brand", async ({ page }) => {
  await page.goto("/?mock=needs-attention");
  await expect(page.getByLabel("Protection status").getByText("Protected", { exact: true })).toBeVisible();
  await expect(page.locator(".agent-block", { hasText: "Codex" }).getByText("Needs attention")).toBeVisible();

  await tab(page, "Activity").click();
  const inspector = page.getByLabel("Request details");
  await expect(inspector.getByText("Select a request")).toBeVisible();
  const list = page.getByRole("list", { name: "Recent requests" });
  await list.getByRole("button", { name: /Unknown client/ }).click();
  await expect(list.getByRole("button", { name: /Unknown client/ })).toHaveAttribute("aria-pressed", "true");
  await expect(inspector.getByText("HTTP 401")).toBeVisible();
  await list.getByRole("button", { name: /Claude Code.*Protected/ }).first().click();
  await expect(inspector.getByText("Signed receipt verified")).toBeVisible();
  await expect(inspector.getByText("rewrote the request")).toBeVisible();

  await tab(page, "Settings").click();
  const privacy = page.getByLabel("Privacy", { exact: true });
  await expect(privacy.getByText("Encrypted outside this device")).toBeVisible();
  await expect(privacy.getByText("2 of 2 recent answers came with a signed receipt")).toBeVisible();
  await privacy.getByText("Technical details").click();
  await expect(privacy.getByText("Hardware attestation is genuine")).toBeVisible();
  await expect(page.getByText("No longer served: openai/gpt-oss-20b")).toBeVisible();
  await expect(page.getByRole("button", { name: "Restore All…" })).toBeEnabled();
  const about = page.getByLabel("About", { exact: true });
  await expect(about.getByRole("img", { name: "Dstack TEE" })).toBeVisible();
  await expect(about.getByText("by Dstack TEE")).toBeVisible();
});

test("no horizontal overflow at 360px and at 200% zoom, no overview scroll at 440x780", async ({ page }) => {
  await page.setViewportSize({ width: 440, height: 780 });
  await page.goto("/?mock=ready");
  await expect(page.getByLabel("Protection status").getByText("Protected", { exact: true })).toBeVisible();
  const fits = await page.evaluate(() => {
    const view = document.querySelector(".content");
    return view !== null && view.scrollHeight <= view.clientHeight;
  });
  expect(fits).toBe(true);

  await page.setViewportSize({ width: 360, height: 780 });
  for (const name of ["Activity", "Settings", "Overview"]) {
    await tab(page, name).click();
    expect(await overflow(page)).toBeLessThanOrEqual(0);
  }
  await page.setViewportSize({ width: 440, height: 780 });
  await page.evaluate(() => document.body.style.setProperty("zoom", "2"));
  for (const name of ["Overview", "Activity", "Settings"]) {
    await tab(page, name).click();
    expect(await overflow(page)).toBeLessThanOrEqual(0);
  }
});
