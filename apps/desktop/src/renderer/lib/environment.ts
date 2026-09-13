import { desktopApi as liveApi } from "../desktop-api";
import { mockApi } from "../mock-api";
import type { DesktopApi } from "../../shared/contracts";

// `?mock=<scenario>` renders the window against canned state for screenshots.
export const query = new URLSearchParams(window.location.search);

export const previewMode = query.has("mock");

export const macOS = previewMode || /Macintosh|Mac OS X/.test(navigator.userAgent);

export const desktopApi: DesktopApi = previewMode ? mockApi(query.get("mock")) : liveApi;
