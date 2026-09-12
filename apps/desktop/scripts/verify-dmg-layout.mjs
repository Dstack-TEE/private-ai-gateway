import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtemp, readFile, realpath, rmdir } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

assert.equal(process.platform, "darwin", "DMG layout verification requires Finder on macOS");
const [dmg] = process.argv.slice(2);
assert.ok(dmg, "Supply the built DMG path");
const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const config = JSON.parse(await readFile(path.join(appRoot, "src-tauri/tauri.brand.conf.json"), "utf8"));
const layout = config.bundle.macOS.dmg;
const mount = await mkdtemp(path.join(tmpdir(), "pap-dmg-layout-"));
let mounted = false;
try {
  execFileSync("hdiutil", ["attach", "-readonly", "-nobrowse", "-mountpoint", mount, path.resolve(dmg)], { timeout: 30_000, stdio: "pipe" });
  mounted = true;
  const output = execFileSync("osascript", ["-", mount, `${config.productName}.app`], {
    encoding: "utf8",
    timeout: 20_000,
    input: `on run argv
      set volumeFolder to POSIX file (item 1 of argv) as alias
      set appName to item 2 of argv
      tell application "Finder"
        open volumeFolder
        delay 1
        set appPoint to position of item appName of volumeFolder
        set folderPoint to position of item "Applications" of volumeFolder
        set windowBounds to bounds of container window of volumeFolder
        set viewOptions to icon view options of container window of volumeFolder
        set iconSize to icon size of viewOptions
        set backdrop to get background picture of viewOptions
        set viewWidth to (item 3 of windowBounds) - (item 1 of windowBounds)
        set viewHeight to (item 4 of windowBounds) - (item 2 of windowBounds)
        close container window of volumeFolder
      end tell
      set backdropPath to POSIX path of (backdrop as alias)
      return (item 1 of appPoint as text) & "," & (item 2 of appPoint as text) & "," & (item 1 of folderPoint as text) & "," & (item 2 of folderPoint as text) & "," & (viewWidth as text) & "," & (viewHeight as text) & "," & (iconSize as text) & linefeed & backdropPath
    end run`,
  });
  const [coordinates, backdrop] = output.trim().split(/\r?\n/);
  const expected = [layout.appPosition.x, layout.appPosition.y, layout.applicationFolderPosition.x, layout.applicationFolderPosition.y, layout.windowSize.width, layout.windowSize.height, 128];
  assert.deepEqual(coordinates.split(",").map(Number), expected, "Finder layout differs from the Tauri configuration");
  const background = path.join(mount, ".background", path.basename(layout.background));
  assert.equal(await realpath(backdrop), await realpath(background), "Finder did not select the branded background");
  assert.deepEqual(await readFile(background), await readFile(path.join(appRoot, "src-tauri", layout.background)));
  console.log(`Verified DMG app/Applications centers, window and icon size: ${coordinates}`);
} finally {
  if (mounted) execFileSync("hdiutil", ["detach", mount], { timeout: 30_000, stdio: "pipe" });
  await rmdir(mount).catch((error) => { if (error.code !== "ENOENT") throw error; });
}
