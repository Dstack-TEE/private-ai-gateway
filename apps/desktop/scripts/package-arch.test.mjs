import assert from "node:assert/strict";
import { test } from "node:test";

import { archInstallScript, archPackageMetadata, archPkgbuild } from "./package-arch.mjs";

test("Arch packages preserve native version ordering and package ownership", () => {
  const desktop = archPackageMetadata("desktop", "1.2.3-beta.4", "x64");
  assert.equal(desktop.version, "1.2.3beta.4");
  assert.equal(desktop.sourceVersion, "1.2.3~beta.4");
  assert.equal(desktop.arch, "x86_64");
  assert.deepEqual(desktop.conflicts, ["private-ai-proxy-cli"]);

  const pkgbuild = archPkgbuild(desktop, "a".repeat(64));
  assert.match(pkgbuild, /depends=.*'webkit2gtk-4\.1'/);
  assert.match(pkgbuild, /provides=\('private-ai-proxy-cli=1\.2\.3beta\.4'\)/);
  assert.match(pkgbuild, /options=\('!strip' '!debug'\)/);

  const install = archInstallScript(desktop.name);
  assert.match(install, /pacman -Qqo/);
  assert.match(install, /pre_upgrade\(\) \{ check_private_ai_proxy_owner; \}/);
  assert.match(install, /pre_remove\(\) \{ check_private_ai_proxy_processes; \}/);
});
