# Private AI Proxy CLI Distribution

There is one user-facing CLI. `pap` is the preferred command,
`private-ai-proxy` is its canonical executable and full-name alias, and `aci` is
a legacy alias kept for existing scripts. All three expose the same commands. The
desktop bundle and standalone CLI distribution do not ship a separate `aci`.
They contain the same three executables:

- `private-ai-proxy`: canonical CLI executable, backend client and integrated ACI
  verifier (normally invoked as `pap serve`).
- `private-ai-proxy-service`: persistent per-user backend.
- `private-ai-proxy-helper`: local agent credential helper.

The backend resolves `private-ai-proxy` and the helper next to the canonical
`private-ai-proxy-service` executable. The CLI resolves `private-ai-proxy-service` next to the canonical
`private-ai-proxy` executable. These sibling paths are a packaging contract, not a PATH
lookup.

## Desktop Packages

| Platform | Desktop package | Executable location | CLI registration |
| --- | --- | --- | --- |
| Windows | NSIS | The three executables are siblings in the selected app directory | The installer calls `private-ai-proxy cli install`. It records ownership only when it inserts a current-user PATH entry. |
| macOS | DMG app | `Private AI Proxy.app/Contents/MacOS` | The app registers the bundled CLI automatically on startup without elevation. |
| Linux | DEB, RPM, or Arch package | `/usr/bin` | The package manager owns all three paths; no registration command is required. |

Windows install, upgrade, and uninstall hooks call `private-ai-proxy --yes service stop`
before replacing or removing files. The installer holds the same `startup.lock`
as CLI startup across stop and file replacement, including after the updater UI
exits. A client that finds the lock held waits out another client's backend
start, but reports an installer or update holding it within a few seconds. They never use `setx`, rewrite an unrelated
PATH entry, kill processes by image name, or elevate themselves. A conflicting
unrelated `private-ai-proxy.exe` aborts installation. The Windows workflow contains native
install/status/uninstall checks, but successful execution on the Windows CI
runner and a fresh interactive-terminal PATH check remain release gates.

On macOS, the app attempts user-level CLI registration asynchronously on startup
after it is launched from a stable location. It does not register while running
from a mounted disk image or App Translocation. Registration does not block the
window or gateway. Settings > Command Line retains startup errors and supports
retrying after the app is moved. Removing the command there disables automatic
registration until the user installs it again.

With the backend running, CLI-only users can disable the same preference with
`pap --yes settings set auto-cli-registration false` before `pap cli uninstall`.

The registration is idempotent and never replaces an unrelated command. To
register manually without opening the app:

```bash
"/Applications/Private AI Proxy.app/Contents/MacOS/private-ai-proxy" cli install
```

The preferred command path is `~/.local/bin/pap`; `private-ai-proxy` and `aci`
are installed beside it. All three resolve to the canonical executable.
Registration does not edit shell startup files, so the user
must add `~/.local/bin` to PATH if needed. `--directory` accepts an existing,
current-user-owned directory that is not writable by other users.
`/usr/local/bin` on macOS is reserved for an already-authorized installer or
administrator context; `private-ai-proxy` never requests elevation itself.

## Standalone CLI

Every platform publishes a portable archive containing the three sibling
executables plus the `pap` and `aci` shortcuts. The optional web UI renderer
is embedded in `private-ai-proxy-service`; it adds no loose runtime files or
native desktop shell. Windows uses ZIP with `.cmd` forwarding scripts; macOS and Linux archives use tar.gz with
symlinks.

Extract each version into a fresh directory rather than overlaying older files.
Run `pap cli install` from a stable extracted location when PATH registration
is wanted.

Every alias tells the executable its name, so `aci` can print its legacy note.
Symlinks (Linux packages, archives and `pap cli install` on macOS and Linux)
carry it in `argv[0]`, and the npm package's `aci` entry passes it as `argv0`
to the native binary. A `.cmd` script cannot set `argv[0]`, so the Windows
shims, written like npm's `cmd-shim` with `setlocal` and `%~dp0`, set
`PRIVATE_AI_PROXY_ALIAS=%~n0` for the executable instead; processes the CLI
starts never inherit it. `pap cli install` replaces the shims earlier releases
wrote, which lack that line, as its own; that migration is removed in 0.3 (see
[Removal in 0.3](configuration.md#removal-in-03)).

Linux also publishes CLI-only DEB, RPM, and Arch Linux packages. They install the three real
executables under `/usr/libexec/private-ai-proxy` and package-owned
`/usr/bin/private-ai-proxy`, `pap` and `aci` symlinks. This relies on `private-ai-proxy` canonicalizing itself before it
locates `private-ai-proxy-service`. [nfpm](https://nfpm.goreleaser.com/) builds
every Linux package, desktop and CLI, in all three formats
(`scripts/package-linux.mjs`). The desktop package includes the CLI, so it
provides `private-ai-proxy-cli`, and each package conflicts with the other; the
DEBs also declare `Replaces` so dpkg swaps one for the other (Debian Policy
7.6.2). RPM and Arch packages declare no `Obsoletes` or `replaces`, which would
swap them on every system upgrade. The packages do not check for running
processes (see [Distribution](distribution.md#updates-by-installation)): the package manager refuses
files another package owns, and an upgrade replaces the executables of a running
backend, which keeps its old build until the next client command restarts it. Run
`pap --yes service stop` as the owning user to switch at once. The in-app updater
pauses the user-owned backend before invoking the native installer and preserves an
active session so protection can resume after fresh verification when the app
restarts.

The web UI renderer comes from `npm run build:web` in `apps/desktop`, which
writes `runtime/web-dist` for the CLI package's default `web-ui` feature. Plain
Cargo builds work without it and print a warning; enabling the web UI in such a
build reports that its assets are not built. `npm run build:sidecars` builds the
bundle before compiling Rust, and CLI packaging refuses to run without it. Mac
App Store sidecars are built without default features, so they omit the web UI.

CI extracts every portable archive and CLI-only native package, then runs the
packaged `private-ai-proxy` with a private temporary app home and loopback port. The smoke
starts the sibling `private-ai-proxy-service`, checks status, persists an appearance setting,
stops it, and verifies repeated query-only status remains `not_running`. It does
not configure a provider or access credentials.

## Release Contract

Releases are cut by merging the release-please PR. The release tag then builds,
publishes and distributes every channel; see
[Release orchestration](distribution.md#release-orchestration). The npm
publisher publishes everything as versions of the one `private-ai-proxy`
package, as `@openai/codex` does. It uploads the six `<version>-<os>-<cpu>`
platform versions first, under per-platform dist-tags. It then waits until the
public registry serves all of them and a fresh install of the wrapper resolves
them. Only then does it publish the wrapper under `latest` or `beta`, because
trusted publishing cannot move a dist-tag after the fact. See
[`apps/desktop/npm/README.md`](../npm/README.md#release) for the full order.

`Desktop Tauri` is also the package-smoke entry point. A manual run on a branch
builds unsigned test packages of the committed version and never creates a
release; `package_only` skips the full verification.

- Tags are `desktop-v<semver>` and titles are `Private AI Proxy v<semver>`.
- Beta versions use `x.y.z-beta.n`; stable versions use `x.y.z`.
- Release notes are the release's `CHANGELOG.md` section. GitHub lists the
  assets, including `SHA256SUMS`.
- Public assets use `private-ai-proxy-<version>-<platform>-<arch>.<format>` or
  `private-ai-proxy-cli-<version>-<platform>-<arch>.<format>`.
- Stable desktop releases become the repository's Latest release. Beta releases
  and updater-feed releases never replace Latest. Stable releases also advance
  the beta updater feed when they are newer than its latest beta.

Windows Authenticode signing is optional. When `WINDOWS_CERTIFICATE` (a base64
PFX) and `WINDOWS_CERTIFICATE_PASSWORD` are configured in the protected
environment, CI imports the certificate only for the Windows package job. Tauri
signs the application, bundled executables, and NSIS installer during packaging;
CI verifies the resulting signatures and removes the certificate afterward.
Unsigned Windows builds are valid release artifacts but may trigger stronger
SmartScreen warnings.

## Updates

The update behavior of every installation is in
[Updates by installation](distribution.md#updates-by-installation). Desktop
DMG, NSIS, DEB and RPM installs update in-app. Arch Linux packages, CLI-only
native packages, npm, and portable archives never modify themselves: `pap doctor`,
the web UI, and (for the Arch desktop package) the desktop app announce a newer
release in the saved channel together with the exact upgrade commands.

AppImage is intentionally unsupported. Its temporary mount cannot provide a
stable executable lifetime for a backend that survives the UI process. Existing
AppImage installations cannot automatically change bundle type and require a
manual migration to a native package.

Arch packages use the standard `.pkg.tar.zst` format for x86_64 and aarch64.
Install or upgrade a downloaded desktop package with:

```bash
sudo pacman -U ./private-ai-proxy-<version>-linux-<arch>.pkg.tar.zst
```

A running backend keeps its old build until the next client command restarts
it; run `pap --yes service stop` as the signed-in user to switch at once.
