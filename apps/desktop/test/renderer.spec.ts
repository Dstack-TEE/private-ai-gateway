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
  await expect(openAtLogin).toHaveAttribute("aria-checked", "false");
  await openAtLogin.click();
  await expect(openAtLogin).toHaveAttribute("aria-checked", "true");

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
  await expect(page.locator(".tray-template-icon")).toHaveCSS("mask-image", /tray-mark/);
  await expect(page.locator(".tray-template-icon")).toHaveCSS("opacity", "1");

  await tray.getByRole("menuitem", { name: "Settings…" }).click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();

  await page.getByRole("button", { name: "Profiles", exact: true }).click();
  const settingsDialog = page.getByRole("dialog", { name: "Profiles" });
  const dialogBox = await settingsDialog.boundingBox();
  expect(dialogBox).not.toBeNull();
  const frameCenter = frameBox!.x + frameBox!.width / 2;
  const dialogCenter = dialogBox!.x + dialogBox!.width / 2;
  expect(Math.abs(frameCenter - dialogCenter)).toBeLessThanOrEqual(2);
  await expect(settingsDialog).toBeFocused();
  await expect(settingsDialog).toHaveCSS("outline-style", "none");

});

test("Profiles keeps its list underneath the profile editor", async ({ page }) => {
  await page.setViewportSize({ width: 620, height: 560 });
  await page.goto("/?mock=no-key&native-dialog=profiles");

  await expect(page.locator(".desktop-window, .sidebar")).toHaveCount(0);
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  await expect(profiles).toBeVisible();
  const box = await profiles.boundingBox();
  expect(box).toEqual({ x: 0, y: 0, width: 620, height: 560 });

  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  await expect(page.locator(".profiles-sheet")).toHaveCount(1);
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor).toBeVisible();
  await expect(page.locator("dialog[open]")).toHaveCount(2);
  await editor.getByRole("button", { name: "Cancel" }).click();
  await expect(page.getByRole("dialog", { name: "Profiles" })).toBeVisible();
});

test("complex dialogs render as native child-window surfaces", async ({ page }) => {
  await page.addInitScript(() => {
    new MutationObserver((mutations) => {
      for (const mutation of mutations) {
        for (const node of mutation.addedNodes) {
          if (node instanceof HTMLElement && /Loading (profiles|privacy verification|Local API settings|usage proof)/.test(node.textContent ?? "")) {
            document.documentElement.dataset.loadingFrameObserved = "true";
          }
        }
      }
    }).observe(document, { childList: true, subtree: true });
  });
  const cases = [
    {
      size: { width: 580, height: 510 },
      path: "/?mock=ready&native-dialog=profile-editor",
      name: "New profile",
      text: "Verify and Save",
    },
    {
      size: { width: 700, height: 680 },
      path: "/?mock=ready&native-dialog=privacy",
      name: "Privacy verification",
      text: "Attested encrypted channel",
    },
    {
      size: { width: 600, height: 680 },
      path: "/?mock=no-key&native-dialog=local-api",
      name: "Local API settings",
      text: "Listen address",
    },
    {
      size: { width: 560, height: 500 },
      path: "/?mock=ready&native-dialog=usage-proof&record=51be02",
      name: "Usage proof",
      text: "Signed receipt verified",
    },
  ] as const;

  for (const entry of cases) {
    await page.setViewportSize(entry.size);
    await page.goto(entry.path);
    await expect(page.locator(".desktop-window, .sidebar")).toHaveCount(0);
    const dialog = page.getByRole("dialog", { name: entry.name });
    await expect(dialog).toContainText(entry.text);
    await expect(page.locator("html")).not.toHaveAttribute("data-loading-frame-observed", "true");
    expect(await dialog.boundingBox()).toEqual({ x: 0, y: 0, ...entry.size });
    if (entry.name === "Usage proof" || entry.name === "Privacy verification") {
      const done = dialog.getByRole("button", { name: "Done", exact: true });
      await expect(done).toBeInViewport();
      const content = dialog.locator(entry.name === "Usage proof" ? ".proof-card" : ".privacy-content");
      expect(await content.evaluate((node) => node.scrollTop)).toBe(0);
      if (entry.name === "Privacy verification") {
        const identifiers = dialog.locator(".identity-grid strong.mono");
        expect(await identifiers.count()).toBeGreaterThan(0);
        expect(await identifiers.evaluateAll((nodes) => nodes.every((node) =>
          getComputedStyle(node).whiteSpace === "normal" && node.scrollWidth <= node.clientWidth,
        ))).toBe(true);
      }
      await content.evaluate((node) => { node.scrollTop = node.scrollHeight; });
      await expect(done).toBeInViewport();
      await expect(dialog.getByRole("list")).toHaveCount(entry.name === "Usage proof" ? 1 : 0);
    }
  }
});

test("regenerating the client key requires an explicit native confirmation", async ({ page }) => {
  await page.goto("/?mock=interactive&native-dialog=local-api");
  const key = page.getByLabel("Client key", { exact: true });
  await expect(key).not.toHaveValue("");
  const original = await key.inputValue();
  page.once("dialog", (dialog) => dialog.dismiss());
  await page.getByRole("button", { name: "Regenerate…" }).click();
  await expect(key).toHaveValue(original);
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Regenerate…" }).click();
  await expect(key).not.toHaveValue(original);
});

test("protection flow, page headers, and focus follow the native desktop contract", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=no-profiles");

  await expect(page).toHaveTitle("Private AI Gateway");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.locator(".gateway-verdict")).toHaveCSS("color", "rgba(29, 29, 31, 0.64)");
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);
  let editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toBeVisible();
  await expect(editor.getByRole("button", { name: "Phala" })).toHaveAttribute("aria-pressed", "true");
  await editor.getByRole("button", { name: "Cancel" }).click();
  await page.getByRole("switch", { name: "Start protection" }).click();
  editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toBeVisible();
  await editor.getByLabel("Phala AI API key").fill("sk-test-123");
  await editor.getByRole("button", { name: "Verify and Save" }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);

  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();

  await nav(page, "Overview").focus();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Agents")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Usage")).toBeFocused();

  await nav(page, "Settings").click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();
  await expect(page.getByRole("button", { name: "Profiles", exact: true })).toContainText("Verified configuration");
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

test("overview shows four agents, four current-session records, truthful copy surfaces, and session totals", async ({ page }) => {
  await page.setViewportSize({ width: 1100, height: 900 });
  await page.goto("/?mock=ready");

  const agentsModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents" }) });
  await expect(agentsModule.locator(".agent-block")).toHaveCount(4);
  await expect(agentsModule.locator(".agent-block").last()).toBeVisible();
  const usageModule = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Recent usage" }) });
  await expect(usageModule.locator(".usage-row")).toHaveCount(4);
  await expect(usageModule.locator(".usage-row").last()).toBeVisible();
  expect(await agentsModule.locator(".module").evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true);
  expect(await usageModule.locator(".module").evaluate((node) => node.scrollHeight <= node.clientHeight)).toBe(true);
  expect(await page.locator(".content").evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
  const spacing = await page.locator(".overview-page").evaluate((node) => {
    const modules = Array.from(node.querySelectorAll(".overview-module"), (item) => item.getBoundingClientRect());
    const surface = node.querySelector(".status-surface")?.getBoundingClientRect();
    if (!surface || modules.length !== 4) throw new Error("Overview modules missing");
    return [modules[0].top - surface.bottom, modules[2].top - modules[0].bottom];
  });
  expect(Math.abs(spacing[0] - spacing[1])).toBeLessThanOrEqual(1);
  await expect(page.locator(".overview-module-title").first()).toHaveCSS("user-select", "none");
  await usageModule.locator(".usage-row").first().click();
  const overviewProof = page.getByRole("dialog", { name: "Usage proof" });
  await expect(overviewProof).toContainText("Signed receipt verified");
  await overviewProof.getByRole("button", { name: "Done" }).click();

  const session = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Usage in this session" }) });
  const localApi = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Local API" }) });
  for (const label of ["Requests", "Tokens", "Cost", "Protected"]) {
    await expect(session.getByText(label, { exact: true })).toBeVisible();
  }
  await expect(session.locator("small")).toHaveCount(0);
  await expect(session.getByText("This session", { exact: true })).toHaveCount(0);
  await expect(localApi.locator(".overview-module-title").getByText("Available", { exact: true })).toBeVisible();
  await expect(localApi.locator(".copy-rows").getByText("Available", { exact: true })).toHaveCount(0);
  await expect(localApi.getByText("for your own tools", { exact: true })).toHaveCount(0);
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
  await expect(localSheet.getByRole("button", { name: "Save" })).toBeEnabled();
  await expect(localSheet).toContainText("Saving briefly restarts protection");
  await localSheet.getByLabel("Port", { exact: true }).fill("4181");
  await localSheet.getByRole("button", { name: "Save", exact: true }).click();
  await expect(localSheet).not.toBeVisible();
  await expect(endpoint).toContainText("4181");
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(session.locator("strong")).toHaveText(["—", "—", "—", "—"]);
  await expect(usageModule.locator(".usage-row")).toHaveCount(0);
  await expect(page.locator(".protection-duration")).toHaveCount(0);
});

test("updates are discovered on launch and installation requires confirmation", async ({ page }) => {
  await page.goto("/?mock=update-available");
  await page.getByRole("button", { name: "Update available", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: "Version 0.2.0 is available" })).toBeVisible();
  page.once("dialog", async (dialog) => {
    expect(dialog.message()).toContain("connected agent configurations will be restored");
    await dialog.dismiss();
  });
  await page.getByRole("button", { name: "Install and Restart…", exact: true }).click();
  await expect(page.getByRole("button", { name: "Install and Restart…", exact: true })).toBeEnabled();
  await expect(page.getByRole("heading", { name: "Updates", exact: true })).toHaveCount(0);
  const about = page.getByRole("region", { name: "About", exact: true });
  await expect(about.getByRole("button", { name: "Install and Restart…", exact: true })).toContainText("v0.1.0");
  await expect(about.getByRole("button", { name: "Documentation", exact: true })).toHaveCSS("border-bottom-width", "1px");
  await expect(about.getByRole("button", { name: "GitHub", exact: true })).toHaveCSS("border-bottom-width", "0px");
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
  await expect(blockedProof.getByText("Request kept on this Mac", { exact: true })).toBeVisible();
  await expect(blockedProof.locator(".proof-flow, .privacy-verdict.state-success")).toHaveCount(0);
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
  await page.getByRole("button", { name: "Profiles", exact: true }).click();
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
  await page.getByRole("button", { name: "Privacy verification", exact: true }).click();
  const privacy = page.getByRole("dialog", { name: "Privacy verification" });
  await expect(privacy.getByText("Attested encrypted channel")).toBeVisible();
  await expect(privacy).toContainText("SPKI-pinned TLS");
  await expect(privacy.locator("details")).toHaveCount(0);
  await expect(privacy.getByRole("heading", { name: "Verification checks" })).toBeVisible();
});

test("overview presents local availability and the active profile without session filler", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=ready");

  const status = page.getByLabel("Protection status");
  await expect(status.getByText("Local API available", { exact: true })).toBeVisible();
  await expect(status.getByText("1 agent connected", { exact: true })).toBeVisible();
  await expect(status.getByText("Confidential AI", { exact: true })).toBeVisible();
  const localHeader = page.locator(".overview-module-title").filter({ has: page.getByRole("heading", { name: "Local API", exact: true }) });
  const badgeOffset = await localHeader.evaluate((header) => {
    const title = header.querySelector("h2")?.getBoundingClientRect();
    const badge = header.querySelector(".state")?.getBoundingClientRect();
    if (!title || !badge) throw new Error("Missing Local API heading or status");
    return Math.abs(title.y + title.height / 2 - badge.y - badge.height / 2);
  });
  expect(badgeOffset).toBeLessThanOrEqual(0.5);
  await expect(status.getByRole("button", { name: "Profiles: RedPill" })).toBeVisible();
  await expect(status.locator(".status-endpoint")).toHaveCount(0);
  await expect(status.locator(".protection-duration")).toHaveText(/00:10:\d{2}/);
  const alignment = await status.evaluate((node) => {
    const facts = Array.from(node.querySelectorAll(".status-local .status-fact"));
    const text = facts.map((fact) => fact.lastElementChild?.getBoundingClientRect().x);
    const icons = facts.map((fact) => fact.firstElementChild?.getBoundingClientRect().width);
    const left = node.querySelector(".status-local .status-heading")?.getBoundingClientRect();
    const right = node.querySelector(".status-remote .status-heading")?.getBoundingClientRect();
    return { text, icons, heights: facts.map((fact) => fact.getBoundingClientRect().height), headings: [left?.y, right?.y] };
  });
  expect(alignment.text[0]).toBe(alignment.text[1]);
  expect(alignment.icons).toEqual([14, 14]);
  expect(alignment.heights).toEqual([18, 18]);
  expect(alignment.headings[0]).toBe(alignment.headings[1]);
  await status.getByRole("button", { name: "Privacy verification", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Privacy verification" })).toBeVisible();
  await page.getByRole("dialog", { name: "Privacy verification" }).getByRole("button", { name: "Done", exact: true }).click();
  await expect(status.getByText(/answers this session/i)).toHaveCount(0);

  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(status.getByRole("button", { name: "Privacy verification", exact: true })).toHaveCount(0);
  await expect(status.getByText("Not connected", { exact: true })).toBeVisible();
  await expect(page.locator(".tray-template-icon")).toHaveCSS("opacity", "0.45");
  for (const name of ["Agents", "Usage", "Settings"]) {
    await nav(page, name).click();
    await expect(page.locator(".page-switch-copy")).toHaveCSS("color", "rgba(29, 29, 31, 0.64)");
  }
  await expect(page.getByRole("button", { name: "Profiles", exact: true })).toHaveCSS("border-bottom-width", "1px");
  await nav(page, "Agents").click();
  const detect = page.getByRole("button", { name: "Detect installed agents" });
  await detect.click();
  await expect(detect).toBeEnabled();

  await page.goto("/?mock=no-key");
  await expect(page.getByLabel("Protection status").getByText("Credential unavailable", { exact: true })).toBeVisible();
  await page.getByRole("switch", { name: "Start protection" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor).toBeVisible();
  await expect(editor.getByLabel("RedPill API key")).toBeVisible();
});

test("installed agents stay ordered and protection state is consistent across pages", async ({ page }) => {
  await page.goto("/?mock=mixed-agents");
  await expect(page.locator(".status-agent-icon")).toHaveCount(4);
  await expect(page.locator(".status-agent-icon.is-disconnected")).toHaveCount(3);
  const preview = page.locator(".overview-module", { has: page.getByRole("heading", { name: "Agents", exact: true }) });
  await expect(preview.locator(".agent-block")).toHaveCount(4);
  await page.getByRole("button", { name: "Agents", exact: true }).click();
  const installed = page.getByRole("region", { name: /^Installed/ });
  await expect(installed.getByRole("heading")).not.toContainText("active");
  const centered = await page.locator(".agent-block .row-title-line").evaluateAll((rows) => rows.every((row) => {
    const name = row.children[0].getBoundingClientRect();
    const status = row.children[1].getBoundingClientRect();
    return Math.abs(name.top + name.height / 2 - status.top - status.height / 2) <= 1;
  }));
  expect(centered).toBe(true);
  const order = await installed.locator(".row-title").allTextContents();
  await installed.getByRole("switch", { name: "Connect Codex" }).click();
  await expect(installed.locator(".row-title")).toHaveText(order);
  const absent = page.getByRole("region", { name: "Not installed" });
  await expect(absent.getByRole("switch")).toHaveCount(0);
  await expect(absent.getByRole("button", { name: "Website" })).toBeVisible();
  const header = page.locator(".page-header");
  await expect(header.getByText("Protected", { exact: true })).toBeVisible();
  const label = await header.locator(".protection-status > span").boundingBox();
  const timer = await header.locator(".protection-duration").boundingBox();
  expect(label && timer && timer.y >= label.y + label.height).toBeTruthy();
  await header.getByRole("switch", { name: "Stop protection" }).click();
  await expect(header.getByText("Not protected", { exact: true })).toBeVisible();
  await expect(header.locator(".protection-duration")).toHaveCount(0);
  await page.getByRole("button", { name: "Overview", exact: true }).click();
  await expect(page.getByText("Local API unavailable", { exact: true })).toBeVisible();
});

test("editing a live profile reconnects, while a failed candidate stays unsaved and unprotected", async ({ page }) => {
  await page.goto("/?mock=ready");
  await page.getByRole("button", { name: "Profiles: RedPill" }).click();
  const profiles = page.getByRole("dialog", { name: "Profiles" });
  await profiles.getByRole("button", { name: "Edit RedPill" }).click();
  const editor = page.getByRole("dialog", { name: "Edit profile" });
  await expect(editor.getByLabel("RedPill API key")).toBeEnabled();
  await expect(editor.getByText(/Saving briefly stops protection/)).toBeVisible();
  await editor.getByRole("button", { name: "Verify and Save" }).click();
  await expect(profiles).toBeVisible();
  await profiles.getByRole("button", { name: "New Profile" }).click();
  const candidate = page.getByRole("dialog", { name: "New profile" });
  await expect(candidate.getByText(/Saving briefly stops protection/)).toBeVisible();
  await candidate.getByRole("button", { name: "Custom", exact: true }).click();
  await candidate.getByLabel("Service endpoint").fill("https://unreachable.invalid");
  await candidate.getByLabel("API key", { exact: true }).fill("sk-test-candidate");
  await candidate.getByRole("button", { name: "Verify and Save" }).click();
  await expect(candidate.getByRole("alert")).toContainText("did not answer");
  await candidate.getByRole("button", { name: "Cancel" }).click();
  await expect(profiles.locator(".profile-select")).toHaveCount(1);
  await profiles.getByRole("button", { name: "Done", exact: true }).click();
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
});

test("a native proof error remains dismissible without a window close button", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=usage-proof&record=missing");
  await expect(page.getByRole("alert")).toHaveText("Usage record not found");
  await expect(page.getByRole("button", { name: "Done", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByLabel("Usage proof closed")).toBeVisible();
});

test("startup preferences and saved agent links are independent of protection", async ({ page }) => {
  await page.goto("/?mock=ready");
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await nav(page, "Agents").click();
  const codex = page.locator(".agent-block", { hasText: "Codex" });
  await codex.getByRole("switch", { name: "Connect Codex" }).click();
  await expect(codex.getByRole("switch", { name: "Disconnect Codex" })).toBeChecked();
  await page.getByRole("switch", { name: "Start protection" }).click();
  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(codex.getByRole("switch", { name: "Disconnect Codex" })).toBeChecked();
  await nav(page, "Settings").click();
  const startup = page.getByRole("region", { name: "General" });
  await expect(startup.getByRole("button", { name: "Profiles", exact: true })).toBeVisible();
  await expect(startup.getByRole("button", { name: "Local API settings", exact: true })).toBeVisible();
  const about = page.getByRole("region", { name: "About" });
  await expect(about.getByRole("button", { name: "Documentation" })).toBeVisible();
  await expect(about.getByRole("button", { name: "GitHub" })).toBeVisible();
  await expect(startup.getByRole("switch", { name: "Open at Login" })).not.toBeChecked();
  await expect(startup.getByRole("switch", { name: "Connect on launch" })).not.toBeChecked();
  await startup.getByRole("switch", { name: "Open at Login" }).click();
  await startup.getByRole("switch", { name: "Connect on launch" }).click();
  await expect(startup.getByRole("switch", { name: "Open at Login" })).toBeChecked();
  await expect(startup.getByRole("switch", { name: "Connect on launch" })).toBeChecked();
  await expect(page.getByRole("switch", { name: "Start protection" })).toBeVisible();
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

  await page.getByRole("button", { name: "Profiles", exact: true }).click();
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

test("fail-closed states stay explicit and never show the success effects", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=blocked");
  const status = page.getByLabel("Protection status");
  await expect(status.getByText("Protection blocked", { exact: true })).toBeVisible();
  await expect(page.getByText(/identity changed after verification/i)).toBeVisible();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toHaveAttribute("aria-checked", "true");
  await expect(page.locator(".tracks-left")).toHaveCSS("opacity", "0");
  await expect(page.locator(".status-glow")).toHaveCSS("opacity", "0");
  for (const name of ["Agents", "Usage", "Settings"]) {
    await nav(page, name).click();
    await expect(page.locator(".page-protection").getByText("Protection blocked", { exact: true })).toBeVisible();
    await expect(page.locator(".page-switch-copy")).toHaveClass(/state-danger/);
  }

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
