import { copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const destination = path.join(appRoot, "native/.generated-assets");
await rm(destination, { recursive: true, force: true });
await mkdir(path.join(destination, "agents"), { recursive: true });
await mkdir(path.join(destination, "providers"), { recursive: true });
await mkdir(path.join(destination, "brand"), { recursive: true });

const agents = {
  codex: "codex-color.svg",
  "claude-code": "claudecode-color.svg",
  opencode: "opencode.svg",
  pi: "pi.svg",
  hermes: "hermesagent.svg",
};
for (const [id, file] of Object.entries(agents)) {
  await copyFile(
    path.join(appRoot, "node_modules/@lobehub/icons-static-svg/icons", file),
    path.join(destination, "agents", `${id}.svg`),
  );
}
await copyFile(
  path.join(appRoot, "src/renderer/assets/service-phala.svg"),
  path.join(destination, "providers/phala.svg"),
);
await copyFile(
  path.join(appRoot, "src/renderer/assets/service-redpill.png"),
  path.join(destination, "providers/redpill.png"),
);
await copyFile(
  path.join(appRoot, "src/renderer/generated/brand-mark-dark.svg"),
  path.join(destination, "brand/mark.svg"),
);
await copyFile(
  path.join(appRoot, "src/renderer/generated/tray-mark.svg"),
  path.join(destination, "brand/private-ai-gateway.svg"),
);
await copyFile(
  path.join(appRoot, "src/renderer/generated/tray-mark-protected.svg"),
  path.join(destination, "brand/private-ai-gateway-protected.svg"),
);
for (const [source, target] of [
  ["trayTemplate@2x.png", "tray.ico"],
  ["trayTemplateProtected@2x.png", "tray-protected.ico"],
]) {
  const png = await readFile(path.join(appRoot, "assets/tray", source));
  await writeFile(path.join(destination, "brand", target), pngIco(png, 36, 36));
}
console.log(`Prepared native assets in ${destination}`);

function pngIco(png, width, height) {
  const header = Buffer.alloc(22);
  header.writeUInt16LE(0, 0);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(1, 4);
  header.writeUInt8(width >= 256 ? 0 : width, 6);
  header.writeUInt8(height >= 256 ? 0 : height, 7);
  header.writeUInt8(0, 8);
  header.writeUInt8(0, 9);
  header.writeUInt16LE(1, 10);
  header.writeUInt16LE(32, 12);
  header.writeUInt32LE(png.length, 14);
  header.writeUInt32LE(header.length, 18);
  return Buffer.concat([header, png]);
}
