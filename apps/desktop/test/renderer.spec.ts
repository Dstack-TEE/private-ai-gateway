import { expect, test } from "@playwright/test";

type Page = import("@playwright/test").Page;

const nav = (page: Page, name: string) =>
  page.getByRole("navigation", { name: "Main navigation" }).getByRole("button", { name });

const overflow = (page: Page) =>
  page.evaluate(() => {
    const content = document.querySelector<HTMLElement>(".content");
    return Math.max(
      document.documentElement.scrollWidth - document.documentElement.clientWidth,
      content ? content.scrollWidth - content.clientWidth : 0,
    );
  });

test("public preview frames the Tauri renderer as a macOS window and exposes the tray contract", async ({ page }) => {
  await page.setViewportSize({ width: 1100, height: 900 });
  await page.goto("/?mock=ready");

  const frame = page.locator(".desktop-window");
  const frameBox = await frame.boundingBox();
  expect(frameBox).not.toBeNull();
  expect(frameBox!.x).toBeGreaterThan(0);
  expect(frameBox!.y).toBeGreaterThanOrEqual(40);
  expect(frameBox!.width).toBeLessThan(1100);
  await expect(page.locator(".traffic-lights > span")).toHaveCount(3);

  await page.getByRole("button", { name: "Private AI Gateway menu" }).click();
  const tray = page.getByRole("menu", { name: "Private AI Gateway" });
  await expect(tray.getByRole("switch", { name: "Stop protection" })).toBeVisible();
  for (const name of ["Open Private AI Gateway", "Settings…", "Quit Private AI Gateway"]) {
    await expect(tray.getByRole("menuitem", { name })).toBeVisible();
  }
  const openAtLogin = tray.getByRole("menuitemcheckbox", { name: "Open at Login" });
  await expect(openAtLogin).toHaveAttribute("aria-checked", "true");
  await openAtLogin.click();
  await expect(openAtLogin).toHaveAttribute("aria-checked", "false");
  await tray.getByRole("menuitem", { name: "Settings…" }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();

  await page.getByRole("button", { name: "Configure…" }).click();
  const dialogBox = await page.getByRole("dialog", { name: "Confidential AI settings" }).boundingBox();
  expect(dialogBox).not.toBeNull();
  const frameCenter = frameBox!.x + frameBox!.width / 2;
  const dialogCenter = dialogBox!.x + dialogBox!.width / 2;
  expect(Math.abs(frameCenter - dialogCenter)).toBeLessThanOrEqual(2);
});

test("protection flow, page headers, and focus follow the native desktop contract", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=interactive");

  await expect(page).toHaveTitle("Private AI Gateway");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();

  await nav(page, "Overview").focus();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Agents")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Usage")).toBeFocused();

  await nav(page, "Settings").click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();
  await expect(page.getByText("Protected", { exact: true })).toBeVisible();

  await page.getByRole("button", { name: "Configure…" }).click();
  const service = page.getByRole("dialog", { name: "Confidential AI settings" });
  await service.getByLabel("RedPill API key").fill("sk-test-123");
  await service.getByRole("button", { name: "Verify", exact: true }).click();
  await expect(service.getByText("Using the saved key")).toBeVisible();
  await service.getByRole("button", { name: "Done" }).click();

  await expect(page.getByRole("switch", { name: "Cancel protection start" })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();

  await nav(page, "Overview").click();
  await expect(page.getByRole("heading", { name: "Overview", level: 1 })).toBeFocused();
  await expect(page.getByLabel("Protection status").getByText("Protected", { exact: true })).toBeVisible();
  await expect(page.locator(".tracks-left")).toHaveCSS("opacity", "0.07");
  await expect(page.locator(".tracks-right")).toHaveCSS("opacity", "0.12");

  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.locator(".tracks-left")).toHaveCSS("opacity", "0");
});

test("five agents use verified discovery, a required Codex default, previews, and reversible restore", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");
  await nav(page, "Agents").click();

  const rows = page.locator(".agent-block");
  await expect(rows).toHaveCount(5);
  for (const name of ["Codex", "Claude Code", "OpenCode", "Pi", "Hermes"]) {
    await expect(rows.filter({ hasText: name })).toBeVisible();
  }

  const codex = rows.filter({ hasText: "Codex" });
  await codex.locator(".inline-switch").click();
  const codexSheet = page.getByRole("dialog", { name: "Connect Codex" });
  await expect(codexSheet.getByLabel("Default model for Codex")).toHaveValue("");
  await expect(codexSheet.getByRole("option", { name: "Select a verified model" })).toBeAttached();
  await expect(codexSheet.getByRole("button", { name: "Connect", exact: true })).toBeDisabled();
  await codexSheet.getByLabel("Default model for Codex").selectOption("openai/gpt-oss-20b");
  await expect(codexSheet.getByText(/available model choices come from the verified service/i)).toBeVisible();
  await codexSheet.getByRole("button", { name: "Connect", exact: true }).click();
  await expect(codex.getByText("Connected", { exact: true })).toBeVisible();

  const pi = rows.filter({ hasText: "Pi" });
  await pi.locator(".inline-switch").click();
  const piSheet = page.getByRole("dialog", { name: "Connect Pi" });
  await expect(piSheet.getByRole("combobox")).toHaveCount(0);
  await expect(piSheet.getByText("Pi discovers the verified model catalog automatically.")).toBeVisible();
  await piSheet.getByRole("button", { name: "Cancel" }).click();
  await expect(pi.getByRole("checkbox", { name: "Connect Pi" })).toBeFocused();

  await nav(page, "Settings").click();
  await page.getByRole("button", { name: "Restore all" }).click();
  const restore = page.getByRole("dialog", { name: "Restore all agents" });
  await restore.getByRole("button", { name: "Restore All" }).click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText("All agent configurations restored");
});

test("overview shows five agents, five current-session records, truthful copy surfaces, and session totals", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 820 });
  await page.goto("/?mock=ready");

  const agentsModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents" }) });
  await expect(agentsModule.locator(".agent-block")).toHaveCount(5);
  const usageModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Recent usage" }) });
  await expect(usageModule.locator(".preview-row")).toHaveCount(5);

  const session = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Session usage" }) });
  for (const label of ["Requests", "Tokens", "Cost", "Protected"]) {
    await expect(session.getByText(label, { exact: true })).toBeVisible();
  }

  const localApi = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Local API" }) });
  const endpoint = localApi.getByRole("button", { name: /Local endpoint/ });
  await endpoint.hover();
  await expect(endpoint.getByText("Copy", { exact: true })).toBeVisible();
  await endpoint.getByText("http://127.0.0.1:4180").click();
  await expect(endpoint.getByText("Copied", { exact: true })).toBeVisible();
  await expect(page.locator('.sr-only[role="status"]')).toContainText("Local endpoint copied");

  const clientKey = localApi.getByRole("button", { name: /Client key/ });
  await clientKey.click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText("Client key copied");
  await localApi.getByRole("button", { name: "Reveal client key" }).click();
  await expect(clientKey).toContainText("pag_demo_");

  await localApi.getByRole("button", { name: "Local API settings" }).click();
  const localSheet = page.getByRole("dialog", { name: "Local API settings" });
  await expect(localSheet).toBeVisible();
  const settingsEndpoint = localSheet.getByRole("button", { name: /OpenAI-style endpoint/ });
  await settingsEndpoint.hover();
  await expect(settingsEndpoint.getByText("Copy", { exact: true })).toBeVisible();
});

test("usage history filters, paginates, inspects proof boundaries, exports, and clears explicitly", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 760 });
  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();

  const history = page.locator('ul[aria-label="Usage history"]');
  await expect(history.getByRole("button")).toHaveCount(20);
  await page.getByRole("button", { name: "Next usage page" }).click();
  await expect(page.getByRole("heading", { name: "Usage history" })).toBeFocused();
  await expect(page.locator(".pagination").getByText(/^Page 2/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Previous usage page" })).toBeEnabled();

  const uncertainDelivery = history.getByRole("button", { name: /Upstream failed/ }).first();
  await uncertainDelivery.click();
  await expect(page.getByLabel("Usage details")).toContainText("whether the service received it could not be confirmed");

  const agentFilter = page.getByRole("combobox", { name: "Agent", exact: true });
  await agentFilter.selectOption("hermes");
  await expect(history.getByRole("button").first()).toContainText("Hermes");
  await agentFilter.selectOption("");
  const blocked = history.getByRole("button", { name: /Blocked locally/ }).first();
  await blocked.click();
  const details = page.getByLabel("Usage details");
  await expect(details.getByText("Blocked locally", { exact: true })).toBeVisible();
  await expect(details.getByText(/did not leave this Mac/)).toBeVisible();

  await page.getByRole("button", { name: "Export usage as CSV" }).click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText(/Exported \d+ usage records/);

  await page.getByRole("button", { name: "Clear usage history" }).click();
  const clear = page.getByRole("dialog", { name: "Clear usage history" });
  await expect(clear.getByText(/permanently deletes the local usage database records/i)).toBeVisible();
  await clear.getByRole("button", { name: "Clear History" }).click();
  await expect(page.locator(".usage-history")).toContainText("No saved usage matches these filters.");
  await expect(page.locator('.sr-only[role="status"]')).toContainText(/Deleted \d+ usage records/);
});

test("model catalog is discovered inside Confidential AI settings, scrollable, priced, and marks only reported TEE models", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();

  await expect(page.locator("details.model-catalog")).toHaveCount(0);
  await page.getByRole("button", { name: "Configure…" }).click();
  const service = page.getByRole("dialog", { name: "Confidential AI settings" });
  const catalog = service.getByRole("region", { name: "Verified model catalog" });
  await expect(catalog.getByText("6 models · scroll for more", { exact: true })).toBeVisible();
  await expect(catalog.locator(".model-row")).toHaveCount(6);
  await expect(catalog.getByText(/\$0\.080 input/).first()).toBeVisible();
  await expect(catalog.getByText(/131K context/).first()).toBeVisible();
  await expect(catalog.getByText(/tools · reasoning/i).first()).toBeVisible();
  await expect(catalog.locator(".badge", { hasText: "TEE" })).toHaveCount(5);
  expect(await catalog.evaluate((node) => node.scrollHeight > node.clientHeight)).toBe(true);
  await service.getByRole("button", { name: "Done" }).click();
  await nav(page, "Overview").click();
  await page.getByRole("button", { name: "Privacy verification" }).click();
  const privacy = page.getByRole("dialog", { name: "Privacy verification" });
  await expect(privacy.getByText("Attested encrypted channel")).toBeVisible();
  await expect(privacy).toContainText("SPKI-pinned TLS");
});

test("fail-closed states stay explicit and never show the success effects", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=blocked");
  const status = page.getByLabel("Protection status");
  await expect(status.getByText("Protection blocked", { exact: true })).toBeVisible();
  await expect(page.getByText(/identity changed after verification/i)).toBeVisible();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toHaveAttribute("aria-checked", "true");
  await expect(page.locator(".tracks-left")).toHaveCSS("opacity", "0");
  await expect(page.locator(".status-glow")).toHaveCSS("opacity", "0");

  await page.goto("/?mock=endpoint-busy");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.getByText(/Address already in use/i)).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeDisabled();
});

test("responsive, zoomed, dark, high-contrast, and reduced-motion layouts stay bounded and readable", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "dark", contrast: "more", reducedMotion: "reduce" });
  await page.goto("/?mock=ready");
  for (const viewport of [
    { width: 940, height: 720 },
    { width: 720, height: 600 },
    { width: 540, height: 720 },
    { width: 320, height: 640 },
  ]) {
    await page.setViewportSize(viewport);
    for (const name of ["Overview", "Agents", "Usage", "Settings"]) {
      await nav(page, name).click();
      expect(await overflow(page), `${name} at ${viewport.width}px`).toBeLessThanOrEqual(0);
    }
  }

  await page.setViewportSize({ width: 940, height: 720 });
  await page.evaluate(() => { document.documentElement.style.zoom = "2"; });
  await nav(page, "Overview").click();
  expect(await overflow(page), "Overview at 200% zoom").toBeLessThanOrEqual(0);
  await expect(page.locator(".track-strip").first()).toHaveCSS("animation-name", "none");

  const audit = await page.evaluate(() => {
    const productText = [...document.querySelectorAll<HTMLElement>("body *")]
      .filter((node) => node.offsetParent !== null && node.childElementCount === 0 && node.textContent?.trim())
      .filter((node) => !node.closest(".track-layer, .sr-only"));
    const tooSmall = productText.filter((node) => Number.parseFloat(getComputedStyle(node).fontSize) < 12);
    const clippedControls = [...document.querySelectorAll<HTMLElement>("button, select, input")]
      .filter((node) => node.offsetParent !== null && (node.scrollWidth > node.clientWidth + 1 || node.scrollHeight > node.clientHeight + 1));
    const nestedInteractive = document.querySelectorAll("button button, button input, button select, a button, label button").length;
    return { tooSmall: tooSmall.map((node) => node.textContent), clippedControls: clippedControls.map((node) => node.getAttribute("aria-label") ?? node.textContent), nestedInteractive };
  });
  expect(audit.tooSmall).toEqual([]);
  expect(audit.clippedControls).toEqual([]);
  expect(audit.nestedInteractive).toBe(0);
});
