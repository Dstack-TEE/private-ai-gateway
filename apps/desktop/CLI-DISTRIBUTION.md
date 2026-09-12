# PAP CLI Distribution

There is one user-facing CLI: `pap`, including all five ACI commands. The
desktop bundle and standalone CLI distribution do not ship a separate `aci`.
They contain the same three executables:

- `pap`: user-facing CLI, backend client and integrated ACI verifier (`pap serve`).
- `pap-service`: persistent per-user backend.
- `private-ai-proxy-helper`: local agent credential helper.

The backend resolves `pap` and the helper next to the canonical
`pap-service` executable. The CLI resolves `pap-service` next to the canonical
`pap` executable. These sibling paths are a packaging contract, not a PATH
lookup.

## Desktop Packages

| Platform | Desktop package | Executable location | CLI registration |
| --- | --- | --- | --- |
| Windows | NSIS | The three executables are siblings in the selected app directory | The installer calls `pap cli install`. It records ownership only when it inserts a current-user PATH entry. |
| macOS | DMG app | `Private AI Proxy.app/Contents/MacOS` | The app registers the bundled CLI automatically on startup without elevation. |
| Linux | DEB or RPM | `/usr/bin` | The package manager owns all three paths; no registration command is required. |

Windows install, upgrade, and uninstall hooks call `pap --yes service stop`
before replacing or removing files. The installer holds the same `startup.lock`
as CLI startup across stop and file replacement, including after the updater UI
exits. They never use `setx`, rewrite an unrelated
PATH entry, kill processes by image name, or elevate themselves. A conflicting
unrelated `pap.exe` aborts installation. The Windows workflow contains native
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
"/Applications/Private AI Proxy.app/Contents/MacOS/pap" cli install
```

The default command path is `~/.local/bin/pap`. The command creates a symlink
to the canonical executable and does not edit shell startup files, so the user
must add `~/.local/bin` to PATH if needed. `--directory` accepts an existing,
current-user-owned directory that is not writable by other users.
`/usr/local/bin` on macOS is reserved for an already-authorized installer or
administrator context; `pap` never requests elevation itself.

## Standalone CLI

Every platform publishes a portable archive containing the three sibling
executables and no desktop UI. Windows uses ZIP; macOS and Linux use tar.gz.
Run `pap cli install` from a stable extracted location when PATH registration
is wanted.

Linux also publishes CLI-only DEB and RPM packages. They install the three real
executables under `/usr/libexec/private-ai-proxy` and a package-owned
`/usr/bin/pap` symlink. This relies on `pap` canonicalizing itself before it
locates `pap-service`. Package lifecycle scripts reject an unrelated owner of
`/usr/bin/pap` and refuse replacement or removal while an exact bundled backend,
verifier, or helper executable is still running. They do not invoke a user backend as
root. Run `pap --yes service stop` as the owning user before a manual package
upgrade; the in-app updater performs that user-context stop before invoking the
native installer.

CI extracts every portable archive and CLI-only native package, then runs the
packaged `pap` with a private temporary app home and loopback port. The smoke
starts the sibling `pap-service`, checks status, persists an appearance setting,
stops it, and verifies repeated query-only status remains `not_running`. It does
not configure a provider or access credentials.

## Updates

AppImage is intentionally unsupported. Its temporary mount cannot provide a
stable executable lifetime for a backend that survives the UI process. Linux
desktop DEB/RPM builds participate in the signed Tauri updater using the
installer-specific `linux-x86_64-deb` and `linux-x86_64-rpm` manifest entries;
the updater verifies the artifact and obtains user authorization for the native
installer. CLI-only DEB/RPM packages continue to update through the system
package manager. Existing AppImage installations cannot automatically change
bundle type and require a manual migration to DEB or RPM.
