import { expect, test } from "@playwright/test";

test("gateway lifecycle, key handling, and agent connect flow", async ({ page }) => {
  await page.goto("/?mock=interactive");
  const claudeRow = page.locator(".agent-block", { hasText: "Claude Code" });

  // Save the API key first (no key saved in the cold state).
  await expect(page.getByText("No API key saved")).toBeVisible();
  await page.getByLabel("RedPill API key").fill("sk-test-123");
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect(page.getByText("API key saved in the system credential store")).toBeVisible();

  // Cancel while verifying returns to the stopped state.
  const url = page.getByLabel("AI service");
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("button", { name: "Start", exact: true })).toBeVisible();

  // A failed verification shows the error; a retry with a good URL recovers.
  await url.fill("https://unreachable.invalid");
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.getByText("did not answer the model list request")).toBeVisible();
  await url.fill("https://tee.redpill.ai");
  await page.getByRole("button", { name: "Start", exact: true }).click();
  await expect(page.getByText("Service verified - requests protected")).toBeVisible();
  await expect(page.getByText("3 from verified service")).toBeVisible();

  // Connect Claude Code: model choice, native modal dialog, apply.
  await claudeRow.getByLabel("Model for Claude Code").selectOption("openai/gpt-oss-20b");
  await claudeRow.getByRole("button", { name: "Connect", exact: true }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByText("Existing secret")).toBeVisible();
  // showModal contains focus: repeated tabbing never reaches a background
  // control (focus stays in the dialog or on the inert document body).
  for (let i = 0; i < 4; i += 1) {
    await page.keyboard.press("Tab");
    const escaped = await page.evaluate(() => {
      const active = document.activeElement;
      return active !== null && active !== document.body && active.closest("dialog") === null;
    });
    expect(escaped).toBe(false);
  }
  await page.mouse.click(5, 5);
  await expect(dialog).toBeVisible();
  await dialog.getByRole("button", { name: "Apply changes" }).click();
  await expect(claudeRow.getByText("Connected", { exact: true })).toBeVisible();

  // Escape closes the disconnect preview and returns focus to the trigger.
  const disconnect = claudeRow.getByRole("button", { name: "Disconnect", exact: true });
  await disconnect.click();
  await expect(page.getByRole("dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("dialog")).toBeHidden();
  await expect(disconnect).toBeFocused();

  // Restore all disconnects every recorded agent through its own dialog.
  await page.getByRole("button", { name: "Restore all", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Restore all" }).click();
  await expect(claudeRow.getByText("Not connected", { exact: true })).toBeVisible();

  // Delete the key and stop; the window returns to the cold state.
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await expect(page.getByText("No API key saved")).toBeVisible();
  await page.getByRole("button", { name: "Stop", exact: true }).click();
  await expect(page.getByRole("button", { name: "Start", exact: true })).toBeVisible();
});

test("no horizontal overflow at 200% zoom", async ({ page }) => {
  await page.setViewportSize({ width: 440, height: 900 });
  await page.goto("/?mock=ready");
  await expect(page.getByText("Service verified - requests protected")).toBeVisible();
  await page.evaluate(() => document.body.style.setProperty("zoom", "2"));
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
});
