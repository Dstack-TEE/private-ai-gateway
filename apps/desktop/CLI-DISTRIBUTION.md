# PAG CLI Distribution

The desktop bundle and the standalone CLI distribution contain the same four
release executables:

- `pag`: user-facing CLI and backend client.
- `pag-service`: persistent per-user backend.
- `aci`: verifier process spawned by `pag-service`.
- `private-ai-gateway-helper`: local agent credential helper.

The backend resolves `aci` and the helper next to the canonical
`pag-service` executable. The CLI resolves `pag-service` next to the canonical
`pag` executable. These sibling paths are a packaging contract, not a PATH
lookup.

## Desktop Packages

| Platform | Desktop package | Executable location | CLI registration |
| --- | --- | --- | --- |
| Windows | NSIS | The four executables are siblings in the selected app directory | The installer calls `pag cli install`. It records ownership only when it inserts a current-user PATH entry. |
| macOS | DMG app | `Private AI Gateway.app/Contents/MacOS` | The app registers the bundled CLI automatically on startup without elevation. |
| Linux | DEB or RPM | `/usr/bin` | The package manager owns all four paths; no registration command is required. |

Windows install, upgrade, and uninstall hooks call `pag --yes service stop`
before replacing or removing files. They never use `setx`, rewrite an unrelated
PATH entry, kill processes by image name, or elevate themselves. A conflicting
unrelated `pag.exe` aborts installation. The Windows workflow contains native
install/status/uninstall checks, but successful execution on the Windows CI
runner and a fresh interactive-terminal PATH check remain release gates.

On macOS, the app attempts user-level CLI registration asynchronously on startup.
Registration does not block the window or gateway. Failures appear in app status,
and Settings > Command Line supports retrying. Removing the command in Settings
disables automatic registration until the user installs it again.

The registration is idempotent and never replaces an unrelated command. To
register manually without opening the app:

```bash
"/Applications/Private AI Gateway.app/Contents/MacOS/pag" cli install
```

The default command path is `~/.local/bin/pag`. The command creates a symlink
to the canonical executable and does not edit shell startup files, so the user
must add `~/.local/bin` to PATH if needed. `--directory` accepts an existing,
current-user-owned directory that is not writable by other users.
`/usr/local/bin` on macOS is reserved for an already-authorized installer or
administrator context; `pag` never requests elevation itself.

## Standalone CLI

Every platform publishes a portable archive containing the four sibling
executables and no desktop UI. Windows uses ZIP; macOS and Linux use tar.gz.
Run `pag cli install` from a stable extracted location when PATH registration
is wanted.

Linux also publishes CLI-only DEB and RPM packages. They install the four real
executables under `/usr/libexec/private-ai-gateway` and a package-owned
`/usr/bin/pag` symlink. This relies on `pag` canonicalizing itself before it
locates `pag-service`. Package lifecycle scripts reject an unrelated owner of
`/usr/bin/pag` and refuse replacement or removal while an exact bundled backend,
ACI, or helper executable is still running. They do not invoke a user backend as
root. Run `pag --yes service stop` as the owning user before a manual package
upgrade; the in-app updater performs that user-context stop before invoking the
native installer.

CI extracts every portable archive and CLI-only native package, then runs the
packaged `pag` with a private temporary app home and loopback port. The smoke
starts the sibling `pag-service`, checks status, persists an appearance setting,
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
