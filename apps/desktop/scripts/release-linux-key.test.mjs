import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { test } from "node:test";

const gpgAvailable = spawnSync("gpg", ["--version"], { stdio: "ignore" }).status === 0;

test("RPM updates only import a downloaded certificate with the pinned primary fingerprint", {
  skip: process.platform !== "linux" || !gpgAvailable,
}, async () => {
  const scratch = await mkdtemp(path.join(tmpdir(), "pap-sign-rpm-update-"));
  const home = path.join(scratch, "gnupg");
  try {
    const bin = path.join(scratch, "bin");
    const log = path.join(scratch, "commands");
    const key = path.join(scratch, "public.asc");
    await mkdir(bin);
    await mkdir(home, { mode: 0o700 });
    await writeFile(path.join(bin, "rpm"), "#!/bin/sh\nexit 1\n", { mode: 0o700 });
    await writeFile(path.join(bin, "rpmkeys"), "#!/bin/sh\nexit 1\n", { mode: 0o700 });
    await writeFile(path.join(bin, "curl"), `#!/bin/sh
set -eu
while [ "$1" != -o ]; do shift; done
cp "$PAP_TEST_PUBLIC" "$2"
`, { mode: 0o700 });
    await writeFile(path.join(bin, "sudo"), `#!/bin/sh
printf '%s %s\\n' "$1" "$2" >> "$PAP_TEST_LOG"
`, { mode: 0o700 });

    const env = {
      ...process.env,
      GNUPGHOME: home,
      PATH: `${bin}:${process.env.PATH}`,
      TMPDIR: scratch,
      PAP_TEST_PUBLIC: key,
      PAP_TEST_LOG: log,
    };
    const runGpg = (...args) => {
      const result = spawnSync("gpg", ["--no-options", "--batch", ...args], { env, encoding: "utf8" });
      assert.equal(result.status, 0, result.stderr);
      return result.stdout;
    };
    runGpg("--pinentry-mode", "loopback", "--passphrase", "", "--quick-generate-key", "Throwaway RPM test <test@example.invalid>", "rsa2048", "cert", "1d");
    const fingerprint = runGpg("--with-colons", "--list-keys").split("\n")
      .find((record) => record.startsWith("fpr:")).split(":")[9];
    await writeFile(key, runGpg("--armor", "--export", fingerprint));
    const script = await readFile(new URL("../core/src/updates/rpm-key.sh", import.meta.url), "utf8");
    const install = (expected) => spawnSync("sh", [
      "-c", script, "pap-rpm-key", expected, "gpg-pubkey-missing",
      path.join(scratch, "missing.asc"), "https://example.invalid/key", "install", "test.rpm",
    ], { env, encoding: "utf8" });

    const rejected = install("0".repeat(40));
    assert.equal(rejected.status, 1);
    assert.match(rejected.stderr, /unexpected primary fingerprint/);
    await assert.rejects(readFile(log), { code: "ENOENT" });

    const accepted = install(fingerprint);
    assert.equal(accepted.status, 0, accepted.stderr);
    assert.equal(await readFile(log, "utf8"), "rpm --import\nrpm -U\n");
  } finally {
    spawnSync("gpgconf", ["--homedir", home, "--kill", "gpg-agent"], { stdio: "ignore" });
    await rm(scratch, { recursive: true, force: true });
  }
});
