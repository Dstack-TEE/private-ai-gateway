import { createBackend } from "#backend";

/** The web UI build (`vite build --mode web`) talks to the backend over HTTP. */
export const web = import.meta.env.MODE === "web";
const backend = createBackend();

const platforms = ["macos", "windows", "linux"] as const;
export type Platform = typeof platforms[number];
/** The desktop app's operating system, from the shell (`appearance-init.js`); a browser has none. */
export const platform = platforms.find((name) => name === document.documentElement.dataset.platform);
/** The desktop app's platform, or in the web UI the browser's. */
export const macOS = platform ? platform === "macos" : /Macintosh|Mac OS X/.test(navigator.userAgent);
/**
 * Only the macOS app overlays the title bar on the window's content, so only
 * there does the content stand in for it; Windows and Linux keep the system
 * title bar, and a browser has its own.
 */
export const overlaidTitleBar = platform === "macos";
export const titleBarDragRegion = overlaidTitleBar ? { "data-tauri-drag-region": true } : {};
export const desktopApi = backend.desktopApi;
export const distributionCapabilities = backend.distributionCapabilities;
/** The browser's sign-in session; only the web build has one. */
export const session = backend.session;
export const windowFocus = backend.windowFocus;
