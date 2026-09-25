import { createBackend } from "#backend";

/** The web UI build (`vite build --mode web`) talks to the backend over HTTP. */
export const web = import.meta.env.MODE === "web";
const backend = createBackend();

export const macOS = /Macintosh|Mac OS X/.test(navigator.userAgent);
export const desktopApi = backend.desktopApi;
export const distributionCapabilities = backend.distributionCapabilities;
/** The browser's sign-in session; only the web build has one. */
export const session = backend.session;
