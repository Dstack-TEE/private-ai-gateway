import type { Appearance, DesktopApi, DistributionCapabilities, GatewayState } from "../../shared/contracts";
import { createBackend } from "#backend";

export const web = import.meta.env.VITE_TARGET === "web";
const backend: {
  desktopApi: DesktopApi;
  distributionCapabilities: DistributionCapabilities;
  initialGatewayState: GatewayState | undefined;
  initialAppearance: Appearance | undefined;
  /** Ends this browser session; only the web build has one. */
  signOut: (() => Promise<void>) | undefined;
} = await createBackend();

export const query = new URLSearchParams(window.location.search);
export const macOS = /Macintosh|Mac OS X/.test(navigator.userAgent);
export const desktopApi = backend.desktopApi;
export const distributionCapabilities = backend.distributionCapabilities;
export const initialGatewayState = backend.initialGatewayState;
export const initialAppearance = backend.initialAppearance;
export const signOut = backend.signOut;
