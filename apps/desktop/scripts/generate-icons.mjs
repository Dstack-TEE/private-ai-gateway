import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { Resvg } from "@resvg/resvg-js";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

await renderSvg(
  path.join(appRoot, "assets/tray/trayTemplate.svg"),
  path.join(appRoot, "assets/tray/trayTemplate.png"),
  18,
);
await renderSvg(
  path.join(appRoot, "assets/tray/trayTemplate.svg"),
  path.join(appRoot, "assets/tray/trayTemplate@2x.png"),
  36,
);
await renderSvg(
  path.join(appRoot, "assets/app-icon.svg"),
  path.join(appRoot, "assets/app-icon.png"),
  1024,
);

async function renderSvg(sourcePath, destinationPath, size) {
  const source = await readFile(sourcePath);
  const image = new Resvg(source, {
    fitTo: {
      mode: "width",
      value: size,
    },
  });
  await writeFile(destinationPath, image.render().asPng());
}
