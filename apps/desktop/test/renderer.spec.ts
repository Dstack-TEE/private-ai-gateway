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

  const brandImageElements = page.locator(".brand-logo img");
  await expect(brandImageElements).toHaveCount(3);
  await expect.poll(() => brandImageElements.evaluateAll((images) =>
    images.every((image) => image instanceof HTMLImageElement && image.complete && image.naturalWidth > 0),
  )).toBe(true);
  const brandIcons = await brandImageElements.evaluateAll((images) =>
    images.map((image) => ({ source: (image as HTMLImageElement).currentSrc })),
  );
  expect(brandIcons).toHaveLength(3);
  expect(brandIcons.every(({ source }) => /brand-mark-(light|dark)/.test(source) && !source.startsWith("data:"))).toBe(true);
  await expect(page.locator(".tray-template-icon")).toHaveClass(/is-protected/);
  await expect(page.locator(".tray-template-icon")).toHaveCSS("mask-image", /tray-mark-protected/);

  await tray.getByRole("menuitem", { name: "Settings…" }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();

  await page.getByRole("button", { name: "Manage…" }).click();
  const settingsDialog = page.getByRole("dialog", { name: "Profiles" });
  const dialogBox = await settingsDialog.boundingBox();
  expect(dialogBox).not.toBeNull();
  const frameCenter = frameBox!.x + frameBox!.width / 2;
  const dialogCenter = dialogBox!.x + dialogBox!.width / 2;
  expect(Math.abs(frameCenter - dialogCenter)).toBeLessThanOrEqual(2);
  await expect(settingsDialog).toBeFocused();
  await expect(settingsDialog).toHaveCSS("outline-style", "none");

});

test("protection flow, page headers, and focus follow the native desktop contract", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=no-profiles");

  await expect(page).toHaveTitle("Private AI Gateway");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();

  await page.getByRole("switch", { name: "Start protection" }).click();
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);
  const editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toBeVisible();
  await expect(editor.getByRole("button", { name: "Phala" })).toHaveAttribute("aria-pressed", "true");
  await editor.getByLabel("Phala AI API key").fill("sk-test-123");
  await editor.getByRole("button", { name: "Verify and Save" }).click();
  await expect(editor).toHaveCount(0);
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  const initialProfilesHeight = await profiles.evaluate((node) => node.getBoundingClientRect().height);
  await expect(profiles.getByText(/Verified configuration/)).toBeVisible();
  await expect(profiles.getByText(/models discovered from the verified endpoint/i)).toHaveCount(0);
  expect(await profiles.evaluate((node) => node.getBoundingClientRect().height)).toBe(initialProfilesHeight);
  const done = profiles.getByRole("button", { name: "Done" });
  await expect(done).not.toHaveClass(/primary/);
  await done.click();

  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();

  await nav(page, "Overview").focus();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Agents")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Usage")).toBeFocused();

  await nav(page, "Settings").click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();
  await expect(page.locator("#service-settings-title + .inset .row-note")).toContainText("Verified configuration");
  await page.getByRole("switch", { name: "Start protection" }).click();
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

test("five agents connect and disconnect directly from the verified discovered catalog", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");
  await nav(page, "Agents").click();

  const rows = page.locator(".agent-block");
  await expect(rows).toHaveCount(5);
  for (const name of ["Codex", "Claude Code", "OpenCode", "Pi", "Hermes"]) {
    await expect(rows.filter({ hasText: name })).toBeVisible();
  }
  const agentImageElements = rows.locator(".mark img");
  await expect(agentImageElements).toHaveCount(5);
  await expect.poll(() => agentImageElements.evaluateAll((images) =>
    images.every((image) => image instanceof HTMLImageElement && image.complete && image.naturalWidth > 0),
  )).toBe(true);
  const iconResults = await agentImageElements.evaluateAll((images) =>
    images.map((image) => ({ source: (image as HTMLImageElement).currentSrc })),
  );
  expect(iconResults).toHaveLength(5);
  expect(iconResults.every(({ source }) => source.includes("/assets/") && !source.startsWith("data:"))).toBe(true);

  const codex = rows.filter({ hasText: "Codex" });
  await codex.getByRole("switch", { name: "Connect Codex" }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(codex.getByText("Connected", { exact: true })).toBeVisible();

  const pi = rows.filter({ hasText: "Pi" });
  await pi.getByRole("switch", { name: "Connect Pi" }).click();
  await expect(pi.getByText("Connected", { exact: true })).toBeVisible();
  await pi.getByRole("switch", { name: "Disconnect Pi" }).click();
  await expect(pi.getByText("Not connected", { exact: true })).toBeVisible();

  await nav(page, "Settings").click();
  page.once("dialog", async (dialog) => {
    expect(dialog.type()).toBe("confirm");
    expect(dialog.message()).toContain("Restore all agents?");
    await dialog.accept();
  });
  await page.getByRole("button", { name: "Restore all" }).click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText("All agent configurations restored");
});

test("overview shows five agents, five current-session records, truthful copy surfaces, and session totals", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 820 });
  await page.goto("/?mock=ready");

  const agentsModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents" }) });
  await expect(agentsModule.locator(".agent-block")).toHaveCount(5);
  await expect(agentsModule.locator(".agent-block").last()).toBeVisible();
  const usageModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Recent usage" }) });
  await expect(usageModule.locator(".usage-row")).toHaveCount(5);
  await expect(usageModule.locator(".usage-row").last()).toBeVisible();
  expect(await agentsModule.locator(".module").evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true);
  expect(await usageModule.locator(".module").evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true);
  await usageModule.locator(".usage-row").first().click();
  const overviewProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(overviewProof).toContainText("Signed receipt verified");
  await overviewProof.getByRole("button", { name: "Done" }).click();

  const session = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Session usage" }) });
  const localApi = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Local API" }) });
  for (const label of ["Requests", "Tokens", "Cost", "Protected"]) {
    await expect(session.getByText(label, { exact: true })).toBeVisible();
  }
  await expect(session.locator("small")).toHaveCount(0);
  for (const module of [localApi, session]) {
    const height = await module.locator(".module").evaluate((node) => node.getBoundingClientRect().height);
    expect(height).toBe(136);
  }

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
  await expect(clientKey).toContainText("sk-pag-");

  await localApi.getByRole("button", { name: "Local API settings" }).click();
  const localSheet = page.getByRole("dialog", { name: "Local API settings" });
  await expect(localSheet).toBeVisible();
  for (const label of ["Listen address", "Allow network access", "Port", "Client host", "Client key"]) {
    await expect(localSheet.getByText(label, { exact: true }).first()).toBeVisible();
  }
  await expect(localSheet.getByRole("button", { name: "Copy OpenAI-style endpoint" })).toBeVisible();
  await expect(localSheet.getByRole("button", { name: "Copy Anthropic-style endpoint" })).toBeVisible();
  await expect(localSheet.getByText("Access keys", { exact: true })).toHaveCount(0);
  await expect(localSheet.getByRole("button", { name: "Save" })).toBeDisabled();
});

test("usage history filters, paginates, inspects proof boundaries, exports, and clears explicitly", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 760 });
  await page.goto("/?mock=ready");
  await nav(page, "Usage").click();

  await expect(page.locator(".chart-column")).toHaveCount(7);
  const chartDays = await page.locator(".chart-column").evaluateAll((nodes) =>
    nodes.map((node) => node.getAttribute("title")?.slice(0, 10)),
  );
  expect(new Set(chartDays).size).toBe(7);

  const history = page.locator('ul[aria-label="Usage history"]');
  await expect(history.getByRole("button")).toHaveCount(20);
  await page.getByRole("button", { name: "Next usage page" }).click();
  await expect(page.getByRole("heading", { name: "Usage history" })).toBeFocused();
  await expect(page.locator(".pagination").getByText(/^Page 2/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Previous usage page" })).toBeEnabled();

  const uncertainDelivery = history.getByRole("button", { name: /Upstream failed/ }).first();
  await uncertainDelivery.click();
  const uncertainProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(uncertainProof).toContainText("whether the service received it could not be confirmed");
  await uncertainProof.getByRole("button", { name: "Done" }).click();

  const agentFilter = page.getByRole("combobox", { name: "Agent", exact: true });
  await agentFilter.selectOption("hermes");
  await expect(history.getByRole("button").first()).toContainText("Hermes");
  await agentFilter.selectOption("");
  const blocked = history.getByRole("button", { name: /Blocked locally/ }).first();
  await blocked.click();
  const blockedProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(blockedProof.getByText("Blocked locally", { exact: true })).toBeVisible();
  await expect(blockedProof.getByText(/did not leave this Mac/)).toBeVisible();
  await blockedProof.getByRole("button", { name: "Done" }).click();
  await expect(history).not.toContainText("/v1/models");

  await page.getByRole("button", { name: "Export usage as CSV" }).click();
  await expect(page.locator('.sr-only[role="status"]')).toContainText(/Exported \d+ usage records/);

  page.once("dialog", async (dialog) => {
    expect(dialog.message()).toContain("Clear usage history?");
    await dialog.dismiss();
  });
  await page.getByRole("button", { name: "Clear usage history" }).click();
  await expect(history.getByRole("button")).toHaveCount(20);

  page.once("dialog", async (dialog) => {
    expect(dialog.type()).toBe("confirm");
    expect(dialog.message()).toContain("Clear usage history?");
    await dialog.accept();
  });
  await page.getByRole("button", { name: "Clear usage history" }).click();
  await expect(page.locator(".usage-history")).toContainText("No saved usage matches these filters.");
  await expect(page.locator('.sr-only[role="status"]')).toContainText(/Deleted \d+ usage records/);
});

test("service settings stay focused while privacy verification exposes the complete proof", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();
  await page.getByRole("switch", { name: "Stop protection" }).click();

  await expect(page.locator("details.model-catalog")).toHaveCount(0);
  await page.getByRole("button", { name: "Manage…" }).click();
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  await expect(profiles.getByText("Model catalog", { exact: true })).toHaveCount(0);
  await expect(profiles.getByText(/Verified configuration/)).toBeVisible();
  const redpillLogo = profiles.locator(".service-redpill img");
  await expect(redpillLogo).toBeVisible();
  await expect.poll(() => redpillLogo.evaluate((image) => (image as HTMLImageElement).currentSrc)).toContain("service-redpill-");
  expect(await redpillLogo.evaluate((image) => (image as HTMLImageElement).currentSrc)).toContain(".png");
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  await expect(page.getByRole("dialog", { name: "Edit profile" }).getByText("Verified configuration", { exact: true })).toBeVisible();
  await page.getByRole("dialog", { name: "Edit profile" }).getByRole("button", { name: "Cancel" }).click();
  await profiles.getByRole("button", { name: "Done" }).click();
  await page.getByRole("switch", { name: "Start protection" }).click();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();
  await nav(page, "Overview").click();
  await page.getByRole("button", { name: "Privacy verification" }).click();
  const privacy = page.getByRole("dialog", { name: "Privacy verification" });
  await expect(privacy.getByText("Attested encrypted channel")).toBeVisible();
  await expect(privacy).toContainText("SPKI-pinned TLS");
  await expect(privacy.locator("details")).toHaveCount(0);
  await expect(privacy.getByRole("heading", { name: "Verification checks" })).toBeVisible();
});

test("Confidential AI presets keep provider credentials scoped and settings stay compact", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");
  await nav(page, "Settings").click();
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();

  const advanced = page.locator("details.settings-advanced");
  await expect(advanced).not.toHaveAttribute("open", "");
  await advanced.locator("summary").click();
  await expect(advanced.getByText("Allow development OS", { exact: true })).toBeVisible();
  const devMode = advanced.getByRole("switch", { name: "Allow development OS" });
  await expect(devMode).toHaveAttribute("aria-checked", "false");
  await devMode.click();
  await expect(page.getByText("Dev mode", { exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toHaveClass(/is-development/);
  await devMode.click();

  const localApi = page.locator("section", { has: page.getByRole("heading", { name: "Local API", level: 2 }) });
  await expect(localApi.getByText("Endpoint", { exact: true })).toHaveCount(0);
  await expect(localApi.getByText("Status", { exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: "Manage…" }).click();
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  const redpill = profiles.locator(".profile-select", { hasText: "RedPill" });
  await expect(redpill).toHaveAttribute("aria-pressed", "true");
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  let editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByRole("button", { name: "RedPill" })).toHaveAttribute("aria-pressed", "true");
  await expect(editor.getByLabel("Service endpoint")).toHaveValue("https://tee.redpill.ai");
  await expect(editor.getByLabel("Service endpoint")).toBeDisabled();

  await editor.getByRole("button", { name: "Phala" }).click();
  await expect(editor.getByLabel("Service endpoint")).toHaveValue("https://inference.phala.com");
  await expect(editor.getByLabel("Phala AI API key")).toBeVisible();
  await expect(editor.getByText("A key is required for a new provider or endpoint.")).toBeVisible();
  await expect(editor.getByRole("button", { name: "Verify and Save" })).toBeDisabled();

  await editor.getByRole("button", { name: "Custom" }).click();
  await expect(editor.getByLabel("Service endpoint")).toBeEnabled();
  await editor.getByLabel("Service endpoint").fill("https://private.example.com");
  await expect(editor.getByLabel("API key")).toBeVisible();
  await editor.getByRole("button", { name: "Cancel" }).click();

  await profiles.getByRole("button", { name: "New Profile" }).click();
  editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor.getByRole("button", { name: "Phala" })).toHaveAttribute("aria-pressed", "true");
  await expect(editor.getByRole("button", { name: "Verify and Save" })).toBeVisible();
  await editor.getByLabel("Profile name").fill("Private Lab");
  await editor.getByRole("button", { name: "Custom" }).click();
  await editor.getByLabel("Service endpoint").fill("https://private.example.com");
  await editor.getByLabel("API key").fill("sk-profile-test");
  await editor.getByRole("button", { name: "Verify and Save" }).click();
  await expect(editor).toHaveCount(0);
  await expect(profiles.locator(".profile-select", { hasText: "Private Lab" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();
  await profiles.locator(".profile-select", { hasText: "RedPill" }).click();
  await expect(profiles.locator(".profile-select", { hasText: "RedPill" })).toHaveAttribute("aria-pressed", "true");
  await profiles.getByRole("button", { name: "Edit Private Lab" }).click();
  editor = page.getByRole("dialog", { name: "Edit profile" });
  page.once("dialog", (dialog) => dialog.accept());
  await editor.getByRole("button", { name: "Delete Profile" }).click();
  await expect(editor).toHaveCount(0);
  await expect(profiles.locator(".profile-select", { hasText: "Private Lab" })).toHaveCount(0);

  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByRole("button", { name: "Delete Profile" })).toBeEnabled();
  page.once("dialog", (dialog) => dialog.accept());
  await editor.getByRole("button", { name: "Delete Profile" }).click();
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);
  await page.getByRole("button", { name: "Manage…" }).click();
  await expect(page.getByRole("dialog", { name: "New profile" })).toBeVisible();
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
  expect(await page.locator(".brand-logo img").first().evaluate((image) => (image as HTMLImageElement).currentSrc)).toContain("brand-mark-dark");

  const audit = await page.evaluate(() => {
    const productText = [...document.querySelectorAll<HTMLElement>("body *")]
      .filter((node) => node.offsetParent !== null && node.childElementCount === 0 && node.textContent?.trim())
      .filter((node) => !node.closest(".track-layer, .sr-only"));
    const tooSmall = productText.filter((node) => Number.parseFloat(getComputedStyle(node).fontSize) < 12);
    const clippedControls = [...document.querySelectorAll<HTMLElement>("button, select, input")]
      .filter((node) => {
        if (node.offsetParent === null) return false;
        const style = getComputedStyle(node);
        const clipsX = node.scrollWidth > node.clientWidth + 1 && style.overflowX !== "hidden";
        const clipsY = node.scrollHeight > node.clientHeight + 1 && style.overflowY !== "hidden";
        return clipsX || clipsY;
      });
    const nestedInteractive = document.querySelectorAll("button button, button input, button select, a button, label button").length;
    return { tooSmall: tooSmall.map((node) => node.textContent), clippedControls: clippedControls.map((node) => node.getAttribute("aria-label") ?? node.textContent), nestedInteractive };
  });
  expect(audit.tooSmall).toEqual([]);
  expect(audit.clippedControls).toEqual([]);
  expect(audit.nestedInteractive).toBe(0);
});
