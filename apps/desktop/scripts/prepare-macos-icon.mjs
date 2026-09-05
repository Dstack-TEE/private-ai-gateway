#!/usr/bin/env node
import { execFileSync } from "node:child_process";
import { copyFile, cp, mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const iconsDir = path.join(appRoot, "src-tauri/icons");
const sourceIcon = path.join(iconsDir, "AppIcon.icon");
const bundledAssets = path.join(iconsDir, "Assets.car");

if (process.platform !== "darwin") {
  await rm(bundledAssets, { force: true });
  console.log("Skipped native macOS icon preparation on this platform");
} else {
  await prepareMacosIcon();
}

async function prepareMacosIcon() {
  requireActool26();
  const scratch = await mkdtemp(path.join(tmpdir(), "private-ai-gateway-icon-"));
  try {
    const compilerIcon = path.join(scratch, "Icon.icon");
    const outputDir = path.join(scratch, "out");
    await cp(sourceIcon, compilerIcon, { recursive: true });
    await mkdir(outputDir);

    // Tauri accepts a precompiled CAR. Compile it here because Tauri 2.11.4
    // unconditionally requests an AccentColor set that this icon-only catalog
    // intentionally does not contain.
    execFileSync(
      "xcrun",
      [
        "actool",
        compilerIcon,
        "--compile",
        outputDir,
        "--output-format",
        "human-readable-text",
        "--notices",
        "--warnings",
        "--output-partial-info-plist",
        path.join(outputDir, "assetcatalog_generated_info.plist"),
        "--app-icon",
        "Icon",
        "--include-all-app-icons",
        "--enable-on-demand-resources",
        "NO",
        "--development-region",
        "en",
        "--target-device",
        "mac",
        "--minimum-deployment-target",
        "26.0",
        "--platform",
        "macosx",
      ],
      { stdio: "inherit" },
    );

    const compiledAssets = path.join(outputDir, "Assets.car");
    const info = JSON.parse(
      execFileSync("xcrun", ["assetutil", "--info", compiledAssets], {
        encoding: "utf8",
      }),
    );
    const appIcon = Array.isArray(info)
      ? info.find(
          (asset) =>
            asset?.AssetType === "Icon Image" &&
            typeof asset.Name === "string" &&
            asset.Name.trim().length > 0,
        )
      : undefined;
    if (!appIcon) {
      throw new Error("actool output does not contain a named macOS app icon");
    }

    await copyFile(compiledAssets, bundledAssets);
    console.log(`Prepared native macOS icon: ${appIcon.Name}`);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
}

function requireActool26() {
  let output;
  try {
    output = execFileSync(
      "xcrun",
      ["actool", "--version", "--output-format=human-readable-text"],
      { encoding: "utf8" },
    );
  } catch (error) {
    throw new Error("Adaptive macOS app icons require Xcode 26 or newer", { cause: error });
  }

  const version = output.match(/^short-bundle-version:\s*(\d+)(?:\.\d+)*\s*$/m)?.[1];
  if (!version || Number.parseInt(version, 10) < 26) {
    throw new Error("Adaptive macOS app icons require Xcode 26 or newer");
  }
}
