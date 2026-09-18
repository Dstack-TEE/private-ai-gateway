import { desktopApi as liveApi } from "../desktop-api";
import type { DesktopApi } from "../../shared/contracts";

export const query = new URLSearchParams(window.location.search);
export const macOS = /Macintosh|Mac OS X/.test(navigator.userAgent);
export const desktopApi: DesktopApi = liveApi;
