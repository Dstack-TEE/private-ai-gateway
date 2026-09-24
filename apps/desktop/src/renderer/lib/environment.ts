import type { DesktopApi, DistributionCapabilities } from "../../shared/contracts";
import { createBackend } from "#backend";

export const web = import.meta.env.VITE_TARGET === "web";
const backend: {
  desktopApi: DesktopApi;
  distributionCapabilities: DistributionCapabilities;
  /** Ends this browser session; only the web build has one. */
  signOut: (() => Promise<void>) | undefined;
} = await createBackend();

export const macOS = /Macintosh|Mac OS X/.test(navigator.userAgent);
export const desktopApi = backend.desktopApi;
export const distributionCapabilities = backend.distributionCapabilities;
export const signOut = backend.signOut;
