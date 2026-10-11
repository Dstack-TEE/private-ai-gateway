// Product identity shown by the renderer; see "Branding" in apps/desktop/README.md.
import { productName } from "../../../src-tauri/tauri.conf.json";
import { BYLINE } from "../../shared/contracts";
import appIconLight from "./app-icon-light.png";
import appIconDark from "./app-icon-dark.png";

export const brand = {
  productName,
  byline: BYLINE,
  appIcon: { light: appIconLight, dark: appIconDark },
} as const;
