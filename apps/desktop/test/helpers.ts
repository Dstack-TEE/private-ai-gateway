export const nav = (page: Page, name: string) =>
  page.getByRole("navigation", { name: "Main navigation" }).getByRole("button", { name });

export async function choose(page: Page, control: import("@playwright/test").Locator, label: string) {
  await control.click();
  await page.getByRole("option", { name: label, exact: true }).click();
}

export type Page = import("@playwright/test").Page;
import { expect } from "@playwright/test";

export async function expectErrorAlert(page: Page, action: () => Promise<unknown>, message: string | RegExp) {
  const pending = page.waitForEvent("dialog", (dialog) => dialog.type() === "alert");
  const operation = action();
  const dialog = await pending;
  expect(dialog.message()).toMatch(message);
  await dialog.accept();
  await operation;
}
