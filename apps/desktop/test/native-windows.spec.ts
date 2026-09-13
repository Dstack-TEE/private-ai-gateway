import { expect, test } from "@playwright/test";
import { nav } from "./helpers";

test("desktop suppresses browser reload menus and preserves native editing actions", async ({ page }) => {
  await page.addInitScript(() => window.addEventListener("mock:edit-menu", (event) => {
    if (event instanceof CustomEvent) document.documentElement.dataset.editMenu = JSON.stringify(event.detail);
  }));
  for (const path of ["/?mock=ready", "/?mock=ready&native-dialog=local-api"]) {
    await page.goto(path);
    await expect(page.getByRole("heading").first()).toBeVisible();
    const prevented = await page.evaluate(() => {
      const context = new MouseEvent("contextmenu", { bubbles: true, cancelable: true });
      document.body.dispatchEvent(context);
      const reloadKeys = [
        { key: "F5" }, { key: "r", metaKey: true },
        { key: "r", ctrlKey: true }, { key: "R", metaKey: true, shiftKey: true },
      ].map((init) => {
        const event = new KeyboardEvent("keydown", { ...init, bubbles: true, cancelable: true });
        window.dispatchEvent(event);
        return event.defaultPrevented;
      });
      const copy = new KeyboardEvent("keydown", { key: "c", ctrlKey: true, bubbles: true, cancelable: true });
      window.dispatchEvent(copy);
      const drop = new DragEvent("drop", { bubbles: true, cancelable: true, dataTransfer: new DataTransfer() });
      drop.dataTransfer?.items.add(new File(["test"], "test.html", { type: "text/html" }));
      window.dispatchEvent(drop);
      return { context: context.defaultPrevented, reloadKeys, copy: copy.defaultPrevented, drop: drop.defaultPrevented };
    });
    expect(prevented).toEqual({ context: true, reloadKeys: [true, true, true, true], copy: false, drop: true });
    await expect(page.locator("html")).not.toHaveAttribute("data-edit-menu");
  }
  await page.getByLabel("Listen address", { exact: true }).click({ button: "right" });
  await expect(page.locator("html")).toHaveAttribute("data-edit-menu", '{"editable":true}');
  await page.getByLabel("Client key", { exact: true }).click({ button: "right" });
  await expect(page.locator("html")).toHaveAttribute("data-edit-menu", '{"editable":false}');
});

test("native event subscriptions isolate child dismissal from the Profiles window", async ({ page }) => {
  await page.addInitScript(() => {
    const state = {
      status: "stopped", configurationVerification: false, apiKeySaved: true,
      config: { remoteUrl: "https://tee.redpill.ai", requireProductionOs: true },
      localApi: { listenAddress: "127.0.0.1", port: 4180, allowNetworkAccess: false },
      activity: [], checks: [], sessionUsage: {}, activeProfileId: "work",
      profiles: [{ id: "work", name: "Work profile", provider: "redpill", remoteUrl: "https://tee.redpill.ai", credentialSaved: true }],
    };
    const callbacks = new Map<number, (event: unknown) => void>();
    const listeners = new Map<number, { event: string; target: { kind: string; label?: string }; handler: number }>();
    let serial = 0;
    Object.defineProperty(window, "__GATEWAY_INITIAL_STATE__", { configurable: true, value: state });
    Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {
      metadata: { currentWebview: { label: "profiles" }, currentWindow: { label: "profiles" } },
      transformCallback: (callback: (event: unknown) => void) => { const id = ++serial; callbacks.set(id, callback); return id; },
      unregisterCallback: (id: number) => callbacks.delete(id),
      invoke: async (command: string, args: { event?: string; target?: { kind: string; label?: string }; handler?: number; eventId?: number }) => {
        if (command === "plugin:event|listen" && args.event && args.handler !== undefined) {
          const id = ++serial;
          listeners.set(id, { event: args.event, target: args.target ?? { kind: "Any" }, handler: args.handler });
          if (args.event === "gateway://dialog-dismissed") document.documentElement.dataset.dismissListenerReady = "true";
          return id;
        }
        if (command === "plugin:event|unlisten" && args.eventId !== undefined) { listeners.delete(args.eventId); return; }
        if (command === "get_gateway_state") return state;
        if (command === "get_appearance") return "system";
        if (command === "get_notification_settings") return { preferences: { enabled: false }, permission: "denied" };
        if (command === "native_dialog_ready") document.documentElement.dataset.presented = "true";
      },
    } });
    window.addEventListener("test:child-dismiss", () => {
      for (const [id, listener] of listeners) {
        if (listener.event === "gateway://dialog-dismissed" && (listener.target.kind === "Any" || listener.target.label === "profile-editor")) {
          callbacks.get(listener.handler)?.({ event: listener.event, id, payload: null });
        }
      }
    });
  });
  await page.goto("/?native-dialog=profiles");
  await expect(page.locator("html")).toHaveAttribute("data-dismiss-listener-ready", "true");
  await expect(page.getByText("Work profile", { exact: true })).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("test:child-dismiss")));
  await expect(page.getByRole("dialog", { name: "Profiles", exact: true })).toBeVisible();
  await expect(page.getByText("Work profile", { exact: true })).toBeVisible();
});

test("native close requests and keyboard close respect a pending save", async ({ page }) => {
  await page.goto("/?mock=local-save-pending&native-dialog=local-api");
  const sheet = page.getByRole("dialog", { name: "Local API settings" });
  await expect(sheet.getByRole("heading").first()).toBeFocused();
  await sheet.getByLabel("Port", { exact: true }).fill("4181");
  await sheet.getByRole("button", { name: "Save", exact: true }).click();
  await expect(sheet.getByRole("button", { name: "Saving…", exact: true })).toBeDisabled();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:native-close")));
  await page.keyboard.press("Meta+w");
  await page.keyboard.press("Escape");
  await page.keyboard.press("Meta+.");
  await expect(sheet).toBeVisible();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-local-save")));
  await expect(sheet).toHaveCount(0);

  await page.goto("/?mock=ready&native-dialog=privacy");
  await expect(page.getByRole("heading", { name: "Privacy verification" })).toBeFocused();
  await page.evaluate(() => window.dispatchEvent(new Event("mock:native-close")));
  await expect(page.getByRole("dialog")).toHaveCount(0);
});

test("reused native dialog discards drafts and credentials before reopening", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=profile-editor");
  const name = page.getByRole("textbox", { name: "Profile name" });
  await name.fill("Unsaved draft");
  await page.getByRole("tab", { name: "API key", exact: true }).click();
  await page.getByLabel("Phala AI API key").fill("sk-test-discard");
  await page.evaluate(() => window.dispatchEvent(new Event("mock:dialog-dismissed")));
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.evaluate(() => window.dispatchEvent(new Event("mock:dialog-open")));
  await expect(name).toHaveValue("Phala");
  await page.getByRole("tab", { name: "API key", exact: true }).click();
  await expect(page.getByLabel("Phala AI API key")).toHaveValue("");
  await name.fill("Another draft");
  await page.evaluate(() => {
    window.dispatchEvent(new Event("mock:dialog-dismissed"));
    window.dispatchEvent(new Event("mock:dialog-open"));
  });
  await expect(name).toHaveValue("Phala");
});

test("native presentation waits for required credentials and local visual assets", async ({ page }) => {
  await page.goto("/?mock=example-key-pending&native-dialog=local-api-example");
  await expect(page.getByRole("dialog", { name: "Local API examples" })).toBeVisible();
  await expect(page.locator("html")).not.toHaveAttribute("data-native-presented", "true");
  await page.evaluate(() => window.dispatchEvent(new Event("mock:finish-example-key")));
  await expect(page.locator("html")).toHaveAttribute("data-native-presented", "true");
  await expect(page.locator("code")).toContainText("sk-pap-");

  await page.addInitScript(() => {
    const decode = HTMLImageElement.prototype.decode;
    HTMLImageElement.prototype.decode = async function () {
      document.documentElement.dataset.imageDecodePending = "true";
      await new Promise<void>((resolve) => window.addEventListener("test:decode-images", () => resolve(), { once: true }));
      return decode.call(this);
    };
  });
  await page.goto("/?mock=ready&native-dialog=profile-editor");
  await expect(page.getByRole("dialog", { name: "New profile" })).toBeVisible();
  await expect(page.locator("html")).toHaveAttribute("data-image-decode-pending", "true");
  await expect(page.locator("html")).not.toHaveAttribute("data-native-presented", "true");
  await page.evaluate(() => window.dispatchEvent(new Event("test:decode-images")));
  await expect(page.locator("html")).toHaveAttribute("data-native-presented", "true");
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
      size: { width: 720, height: 560 },
      path: "/?mock=ready&native-dialog=local-api-example",
      name: "Local API examples",
      text: "sk-pap-",
    },
    {
      size: { width: 580, height: 560 },
      path: "/?mock=ready&native-dialog=profile-editor",
      name: "New profile",
      text: "Sign in with Phala",
    },
    {
      size: { width: 700, height: 680 },
      path: "/?mock=ready&native-dialog=privacy",
      name: "Privacy verification",
      text: "Attested encrypted channel",
    },
    {
      size: { width: 600, height: 512 },
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
    {
      size: { width: 620, height: 560 },
      path: "/?mock=ready&native-dialog=profiles",
      name: "Profiles",
      text: "New Profile",
    },
    {
      size: { width: 580, height: 580 },
      path: "/?mock=ready&native-dialog=notifications",
      name: "Notifications",
      text: "Allow notifications",
    },
  ] as const;

  for (const entry of cases) {
    await page.setViewportSize(entry.size);
    await page.goto(entry.path);
    await expect(page.locator(".desktop-window, .sidebar")).toHaveCount(0);
    const dialog = page.getByRole("dialog", { name: entry.name });
    await expect(dialog).toContainText(entry.text);
    await expect(page.locator("html")).toHaveAttribute("data-native-presented", "true");
    await expect(page.locator("html")).not.toHaveAttribute("data-loading-frame-observed", "true");
    expect(await dialog.evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
    await expect(dialog.locator(".sheet-footer")).toBeInViewport();
    if (entry.name === "New profile" || entry.name === "Local API settings") {
      expect(await dialog.locator(".sheet-scroll").evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
    }
    await expect(dialog.getByRole("heading").first()).toBeFocused();
    expect(await dialog.getByRole("heading").first().evaluate((node) => getComputedStyle(node).boxShadow)).not.toMatch(/(?:^|\s)[1-9]\d*(?:\.\d+)?px/);
    if (entry.name === "Usage proof") {
      const alignment = await dialog.evaluate((node) => {
        const heading = node.querySelector(".sheet-heading")?.getBoundingClientRect();
        const body = node.querySelector(".proof-card")?.getBoundingClientRect();
        return [heading?.x, body?.x, heading?.right, body?.right];
      });
      expect(alignment[0]).toBe(alignment[1]);
      expect(alignment[2]).toBe(alignment[3]);
    }
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
      await expect(dialog.getByRole("list")).toHaveCount(0);
      if (entry.name === "Usage proof") {
        await expect(dialog.locator("svg")).toHaveCount(1);
        await expect(dialog.getByRole("region", { name: "Proof scope" })).toBeVisible();
      }
    }
    await page.setViewportSize({ width: entry.size.width, height: 420 });
    expect(await dialog.evaluate((node) => node.scrollHeight - node.clientHeight)).toBe(0);
    await expect(dialog.locator(".sheet-footer")).toBeInViewport();
  }
});

test("rotating the client key requires an explicit native confirmation", async ({ page }) => {
  await page.goto("/?mock=interactive&native-dialog=local-api");
  const key = page.getByLabel("Client key", { exact: true });
  await expect(key).not.toHaveValue("");
  await expect(page.locator(".sheet-card")).toHaveCount(0);
  const keyGroup = page.locator('[data-slot="input-group"]', { has: key });
  await expect(keyGroup.getByRole("button")).toHaveCount(3);
  await keyGroup.getByRole("button", { name: "Reveal client key" }).click();
  await expect(key).toHaveAttribute("type", "text");
  await keyGroup.getByRole("button", { name: "Hide client key" }).click();
  await expect(key).toHaveAttribute("type", "password");
  await expect(page.getByRole("switch", { name: "Allow network access" })).toHaveCount(0);
  await page.getByLabel("Listen address", { exact: true }).fill("192.168.1.20");
  await page.keyboard.press("Escape");
  await expect(page.getByRole("button", { name: "Network access warning" })).toBeVisible();
  await page.getByLabel("Listen address", { exact: true }).fill("127.0.0.1");
  await page.keyboard.press("Escape");
  const original = await key.inputValue();
  page.once("dialog", (dialog) => dialog.dismiss());
  await page.getByRole("button", { name: "Rotate key" }).click();
  await expect(key).toHaveValue(original);
  page.once("dialog", (dialog) => dialog.accept());
  await page.getByRole("button", { name: "Rotate key" }).click();
  await expect(key).not.toHaveValue(original);
  for (const surface of ["&native-dialog=local-api", ""]) {
    await page.goto(`/?mock=key-rotation-error${surface}`);
    if (!surface) await page.getByRole("button", { name: "Local API settings", exact: true }).click();
    await expect(key).not.toHaveValue("");
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Rotate key" }).click();
    await expect(key).toHaveValue("");
    await expect(page.getByRole("button", { name: "Copy client key" })).toBeDisabled();
    await expect(page.getByRole("button", { name: "Rotate key" })).toBeEnabled();
    await expect(page.getByRole("alert")).toContainText("Could not store the replacement client key");
    page.once("dialog", (dialog) => dialog.accept());
    await page.getByRole("button", { name: "Rotate key" }).click();
    await expect(key).toHaveValue(/^sk-pap-/);
    await expect(page.getByRole("button", { name: "Copy client key" })).toBeEnabled();
    await expect(page.getByRole("alert")).toHaveCount(0);
  }
});

test("protection flow, page headers, and focus follow the native desktop contract", async ({ page }) => {
  await page.setViewportSize({ width: 940, height: 720 });
  await page.goto("/?mock=no-profiles");

  await expect(page).toHaveTitle("Private AI Proxy");
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);
  let editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toHaveCount(0);
  await page.getByRole("switch", { name: "Start protection" }).click();
  editor = page.getByRole("dialog", { name: "New profile" });
  await expect(editor).toBeVisible();
  await expect(editor.getByRole("button", { name: "Phala", exact: true })).toHaveAttribute("aria-pressed", "true");
  await editor.getByRole("tab", { name: "API key", exact: true }).click();
  await editor.getByLabel("Phala AI API key").fill("sk-test-123");
  await editor.getByRole("button", { name: "Save" }).click();
  await expect(editor).toHaveCount(0);
  await expect(page.getByRole("dialog", { name: "Profiles" })).toHaveCount(0);

  await expect(page.getByLabel("Protection status").getByText("Protected", { exact: true })).toBeVisible();
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();

  await nav(page, "Overview").focus();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Agents")).toBeFocused();
  await page.keyboard.press("ArrowDown");
  await expect(nav(page, "Usage")).toBeFocused();

  await nav(page, "Settings").click();
  await expect(page.getByRole("heading", { name: "Settings", level: 1 })).toBeFocused();
  await expect(page.getByRole("button", { name: "Profiles", exact: true })).toContainText("Protected");
  await expect(page.getByRole("switch", { name: "Stop protection" })).toBeVisible();

  await nav(page, "Overview").click();
  await expect(page.getByRole("heading", { name: "Overview", level: 1 })).toBeFocused();
  await expect(page.getByLabel("Protection status").getByText("Protected", { exact: true })).toBeVisible();
  await expect(page.locator(".tracks-left")).toHaveCount(0);

  await page.getByRole("switch", { name: "Stop protection" }).click();
  await expect(page.getByLabel("Protection status").getByText("Not protected", { exact: true })).toBeVisible();
});

test("a native proof error remains dismissible without a window close button", async ({ page }) => {
  await page.goto("/?mock=ready&native-dialog=usage-proof&record=missing");
  await expect(page.getByRole("alert")).toHaveText("Usage record not found");
  await expect(page.getByRole("button", { name: "Done", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByLabel("Usage proof closed")).toBeVisible();
});
