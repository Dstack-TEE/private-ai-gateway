import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const installerScripts = [
  "deb-pre-install.sh",
  "rpm-pre-install.sh",
  "linux-pre-remove.sh",
];

const harness = String.raw`
set -eu
case "$PROCESS_CASE" in
  ancestor)
    desktop_pid=424242
    parent_pid=$desktop_pid
    ;;
  unrelated)
    desktop_pid=424243
    parent_pid=1
    ;;
  *) exit 2 ;;
esac
mkdir -p "$PRIVATE_AI_PROXY_PROC_ROOT/$$" "$PRIVATE_AI_PROXY_PROC_ROOT/$desktop_pid"
printf '%s (package script) S %s\n' "$$" "$parent_pid" > "$PRIVATE_AI_PROXY_PROC_ROOT/$$/stat"
printf '%s (private-ai-proxy-desktop) S 1\n' "$desktop_pid" > "$PRIVATE_AI_PROXY_PROC_ROOT/$desktop_pid/stat"
ln -s "$DESKTOP_EXECUTABLE" "$PRIVATE_AI_PROXY_PROC_ROOT/$desktop_pid/exe"
. "$INSTALLER_SCRIPT"
`;

test("Linux package scripts ignore their updater ancestor but reject an unrelated desktop", { skip: process.platform !== "linux" }, async () => {
  const fixture = await mkdtemp(path.join(os.tmpdir(), "pap-installer-lifecycle-"));
  try {
    const installRoot = path.join(fixture, "install");
    const binaryDirectory = path.join(installRoot, "usr/libexec/private-ai-proxy");
    const commandDirectory = path.join(installRoot, "usr/bin");
    const toolDirectory = path.join(fixture, "tools");
    const cli = path.join(binaryDirectory, "private-ai-proxy");
    const desktop = path.join(binaryDirectory, "private-ai-proxy-desktop");
    await mkdir(binaryDirectory, { recursive: true });
    await mkdir(commandDirectory, { recursive: true });
    await mkdir(toolDirectory, { recursive: true });
    await writeFile(cli, "fixture");
    await writeFile(desktop, "fixture");
    await symlink("../libexec/private-ai-proxy/private-ai-proxy", path.join(commandDirectory, "private-ai-proxy"));
    await writeFile(
      path.join(toolDirectory, "dpkg-query"),
      "#!/bin/sh\nprintf '%s: fixture\\n' \"$DPKG_MAINTSCRIPT_PACKAGE\"\n",
    );
    await writeFile(path.join(toolDirectory, "rpm"), "#!/bin/sh\nprintf '%s\\n' '@PACKAGE_NAME@'\n");
    await chmod(path.join(toolDirectory, "dpkg-query"), 0o755);
    await chmod(path.join(toolDirectory, "rpm"), 0o755);

    for (const scriptName of installerScripts) {
      const installer = path.join(appRoot, "src-tauri/installer", scriptName);
      for (const processCase of ["ancestor", "unrelated"]) {
        const procRoot = path.join(fixture, `proc-${scriptName}-${processCase}`);
        await mkdir(procRoot);
        const result = spawnSync("/bin/sh", ["-c", harness], {
          encoding: "utf8",
          env: {
            ...process.env,
            PATH: `${toolDirectory}:${process.env.PATH ?? ""}`,
            DESKTOP_EXECUTABLE: desktop,
            DPKG_MAINTSCRIPT_PACKAGE: "private-ai-proxy",
            INSTALLER_SCRIPT: installer,
            PRIVATE_AI_PROXY_INSTALL_ROOT: installRoot,
            PRIVATE_AI_PROXY_PROC_ROOT: procRoot,
            PROCESS_CASE: processCase,
          },
        });
        if (processCase === "ancestor") {
          assert.equal(result.status, 0, `${scriptName}: ${result.stderr}`);
        } else {
          assert.equal(result.status, 1, `${scriptName}: ${result.stderr}`);
          assert.match(result.stderr, /Private AI Proxy is still running/);
        }
      }
    }
  } finally {
    await rm(fixture, { recursive: true, force: true });
  }
});
