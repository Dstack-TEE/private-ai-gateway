// Product identity shown by the renderer; see "Branding" in apps/desktop/README.md.
// core/src/brand.rs tests that the byline and service default match its own.
import { productName } from "../../../src-tauri/tauri.conf.json";
import appIconLight from "./app-icon-light.png";
import appIconDark from "./app-icon-dark.png";

export const brand = {
  productName,
  byline: "by dstack TEE",
  service: { defaultUrl: "https://tee.redpill.ai" },
  appIcon: { light: appIconLight, dark: appIconDark },
} as const;
