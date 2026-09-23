# Private AI Proxy CLI Distribution

There is one user-facing CLI. `pap` is the preferred command,
`private-ai-proxy` is its canonical executable and full-name alias, and `aci` is
the protocol-focused alias. All three expose the same commands. The
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
exits. They never use `setx`, rewrite an unrelated
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
`pap --yes settings set autoCliRegistration false` before `pap cli uninstall`.

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

Linux also publishes CLI-only DEB, RPM, and Arch Linux packages. They install the three real
executables under `/usr/libexec/private-ai-proxy` and a package-owned
`/usr/bin/private-ai-proxy` symlink. This relies on `private-ai-proxy` canonicalizing itself before it
locates `private-ai-proxy-service`. Package lifecycle scripts reject an unrelated owner of
`/usr/bin/private-ai-proxy` and refuse replacement or removal while an exact bundled backend,
verifier, or helper executable is still running. They do not invoke a user backend as
root. Run `pap --yes service stop` as the owning user before a manual package
upgrade. The in-app updater pauses the user-owned backend before invoking the native
installer and preserves an active session so protection can resume after fresh
verification when the app restarts.

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

`Desktop stable release` is the coordinated stable production entry. It validates
the request once, uploads the matching Mac App Store build, then calls `Desktop
Tauri` for all Direct targets from the same commit and version. The Direct worker
publishes the GitHub release and updater feed, then dispatches the dedicated npm
publisher at the immutable release tag and waits for its result. npm validates
the top-level workflow filename for trusted publishing, so this keeps the single
configured OIDC publisher without storing a write token or duplicating packaging
logic. The npm publisher uploads the six platform versions first, waits until the
public registry serves all of them and a fresh install of the wrapper resolves
them, and only then publishes the wrapper under `latest` or `beta`, because
trusted publishing cannot move a dist-tag after the fact. See
[`apps/desktop/npm/README.md`](../npm/README.md#release) for the full order.

`Desktop Tauri` remains the reusable Direct release worker and the focused beta,
package-smoke and recovery entry point. Supplying `release_version` enables updater
signing, macOS Developer ID signing and notarization, and the protected
`desktop-release` environment. A run without a release version creates test
packages only; `package_only` cannot be combined with a version.

- Tags are `desktop-v<semver>` and titles are `Private AI Proxy v<semver>`.
- Beta versions use `x.y.z-beta.n`; stable versions use `x.y.z`.
- Stable releases must run from `main`, include all six platform builds, and
  provide a non-empty `release_summary`.
- Release notes always contain status, `What's changed`, desktop downloads,
  standalone CLI downloads, checksums, updater integrity, and build provenance.
- Public assets use `private-ai-proxy-<version>-<platform>-<arch>.<format>` or
  `private-ai-proxy-cli-<version>-<platform>-<arch>.<format>`.
- Stable desktop releases become the repository's Latest release. Beta releases
  and updater-feed releases never replace Latest. Beta and stable updater feeds
  remain independent.

`release_summary` accepts Markdown paragraphs or list items, not headings. Pass
multiline summaries with `gh workflow run -f release_summary="$summary"`; the
GitHub Actions form exposes this input as a single-line field.
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

The lifecycle guard refuses replacement or removal while the per-user backend
is running. Stop it as the signed-in user before upgrading manually.
