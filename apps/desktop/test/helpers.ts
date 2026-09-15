export const nav = (page: Page, name: string) =>
  page.getByRole("navigation", { name: "Main navigation" }).getByRole("button", { name });

export async function choose(page: Page, control: import("@playwright/test").Locator, label: string) {
  await control.click();
  await page.getByRole("option", { name: label, exact: true }).click();
}

export type Page = import("@playwright/test").Page;
