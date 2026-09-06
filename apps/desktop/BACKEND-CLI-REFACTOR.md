# Backend and CLI Refactor

Baseline: PR #177 (`feat/private-ai-gateway-native-clients`), checked at
`2af6240e0fb92665a21d1c20041c76cd138420c8` on 2026-09-06.

## Target

The Tauri UI and a standalone `pag` CLI will connect to one user-owned backend.
That backend will own configuration, credentials, agent projection, usage,
listeners, and ACI children. Closing either client must not stop the backend.
The CLI-only distribution must not depend on Tauri or an installed UI.

This branch starts that work; it does not yet ship a CLI or IPC service.
Unlike the older desktop branch, this baseline has no standalone stdio service
or shared wire protocol. Do not restore those implementations wholesale.

## First Slice: Instance Ownership

`DesktopRuntime::launch` acquires the instance lock before loading settings,
opening usage storage, binding listeners, or initializing tokens. Failure to
acquire the lock returns an error; it no longer constructs a secondary runtime.
A listener bind failure remains a recoverable error in the owning runtime.
Tauri's existing single-instance plugin remains responsible for ordinary
second-launch window activation.

The instance lock owns its file handle rather than leaking a borrowed lock.
Rust's file locking API is stable in the existing Rust 1.89 minimum version.
The agent transaction lock continues to use `fd-lock`; a compatibility test
checks that the new instance lock excludes the old implementation both ways.
The lock file is not unlinked: removing a held lock path would allow another
process to lock a different file at the same path.

The subprocess regression exercises both an already-held lock and a lock-open
failure in an isolated app home. Neither path may create configuration, usage,
or token files. No real credentials or agent configurations are needed.

## Next Slices

1. Define typed, versioned control requests and a shared Rust client. Audit the
   current lifecycle mutex and side-effecting agent queries before allowing
   concurrent clients. Keep wire errors sanitized and requests bounded.
2. Add a foreground backend and local-only IPC: Unix sockets with peer identity
   checks; Windows named pipes with explicit user ACLs and remote rejection.
   Client disconnect releases only connection resources. Service shutdown must
   await owned tasks and children; `kill_on_drop` does not prove crash cleanup.
3. Add `pag status`, `service run/start/stop`, and gateway `start/stop`. Query-only
   commands do not launch a backend. Start waits for an authenticated readiness
   handshake; incompatible versions never replace a running backend implicitly.
4. Connect Tauri and tray actions through the shared client, preserving the
   existing renderer and adding reconnect/unavailable states. Revalidate updater
   shutdown because UI exit will no longer imply backend exit.
5. Expose profiles, agent preview/apply, models, usage, settings, and explicit
   credential operations. Preserve existing verification and rollback policy.
6. Package CLI/backend with Desktop and separately without UI. Add installer-owned
   PATH registration, installation conflict handling, and coordinated upgrades.

## Platform Gates

- macOS DMG drag-and-drop does not install a command in PATH. Provide an explicit
  registration action, or a separate signed PKG pipeline for install-time
  registration. Evaluate SMAppService for optional bundled background services;
  it does not replace CLI registration or define CLI-only packaging.
- Windows NSIS supports install/uninstall hooks. Use explicit per-user PATH
  registration without rewriting the whole PATH; test in a fresh terminal.
  Default named-pipe ACLs are insufficient for the management channel.
- Linux DEB/RPM should own executable paths. Secret Service can depend on an
  unlocked user login session: UI-independent use is not a promise of unattended
  server support. Never fall back to plaintext credential storage.
- CLI registration, user-requested backend startup, and login autostart are
  separate choices. No privileged system daemon is required for this design.
- Native security, installed UI/CLI synchronization, upgrade, and process cleanup
  require real-OS acceptance. Cross-compilation or renderer mocks are not proof.

## Official References

Checked on 2026-09-06; recheck version-sensitive contracts before implementation.

- [Rust File locking](https://doc.rust-lang.org/1.89.0/std/fs/struct.File.html#method.try_lock)
- [VS Code CLI installation](https://code.visualstudio.com/docs/editor/command-line)
- [Tauri sidecar bundling](https://v2.tauri.app/develop/sidecar/)
- [Tauri Windows installer hooks](https://v2.tauri.app/distribute/windows-installer/)
- [Apple SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)
- [Windows named-pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
- [XDG runtime directories](https://specifications.freedesktop.org/basedir/latest/)
- [Tokio graceful shutdown](https://tokio.rs/tokio/topics/shutdown)
- [Secret Service login-session contract](https://specifications.freedesktop.org/secret-service/latest/ch01.html)
