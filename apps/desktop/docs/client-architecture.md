# Private AI Proxy architecture

Status: implemented.

## Responsibilities

| Component | Responsibility | Implementation |
| --- | --- | --- |
| Private AI Gateway | Remote attested inference service and signed receipts | `src/aggregator`, `src/middleware` |
| Private AI Proxy | Desktop profiles, agent connections, verification and usage | `apps/desktop` |
| Local backend | Sessions, configuration transactions, local API and process ownership | `apps/desktop/runtime`, `apps/desktop/gateway` |
| `private-ai-proxy` | Unified managed-client and ACI protocol commands | `apps/desktop/cli` |
| `private-ai-proxy-service` | Per-user backend entry point | `apps/desktop/cli/service.rs` |

## Project boundary

`private-ai-proxy` composes the managed CLI's Clap command tree with its ACI
commands. Management command execution and output live in
`apps/desktop/runtime/src/cli`; ACI command modules live in `apps/desktop/cli`.
There is one PAP user-facing executable and one PAP relying-party verifier.
Desktop integration adds lifecycle events for process integration and post-delivery receipt auditing.
The Private AI Proxy package owns the user-facing CLI, its managed service binary,
and the ACI library modules under `apps/desktop/cli/aci`. Its managed-client
feature adds the desktop runtime only for the executable targets.

Private AI Gateway and Private AI Proxy are independent projects. Gateway owns
its service-side ACI implementation; PAP owns its relying-party verification,
audit, and local-proxy implementation. Neither Rust package imports the other,
their tests do not reach across project directories, and each project has its own
workspace and lockfile. They interoperate only through the published ACI and HTTP
wire protocols. The `aci` command remains an alias of the single
`private-ai-proxy` CLI, not a separate executable or crate.

`private-ai-proxy verify/audit/sessions/send` do not initialize the managed backend or
credential store. `private-ai-proxy serve` streams responses immediately and
audits receipts afterward by default. Receipt checks never gate streaming.
`private-ai-proxy --json serve`
emits lifecycle JSON events.

`private-ai-proxy start/stop` retain managed profiles, user-session continuity and reversible
agent configuration. The backend's supervised verifier process now runs
`private-ai-proxy serve`. Ownership-pipe and child-reaping
behavior is preserved; a backend crash must not leave a verifier listening.
Packages contain `private-ai-proxy`, `private-ai-proxy-service` and the credential
helper. They do not contain an independent `aci` executable.

## Module boundaries

- Renderer `index.tsx` owns bootstrap; `app.tsx` composes the window. `features/`
  contains pages and forms, `windows/` adapts them to native windows, `hooks/`
  owns reusable interactions, and `lib/` contains presentation rules and the
  live desktop API binding. Features never import the app or windows.
- Runtime `controller.rs` owns shared state and launch; its private modules group
  lifecycle, profiles, account login, credentials, agents and local endpoints.
  The same locks and transaction guards span these implementation modules.
- Agent registry, provider data, projection, discovery and validation are separate
  modules. Apply, disconnect, recovery and rollback stay together in transactions.
- OAuth provider/HTTP/billing helpers and verifier events are separate from their
  session owners. Tauri command modules adapt the shared runtime to IPC.
- Core tests are grouped by behavior. Layout, color and asset-name assertions are
  excluded; authorization, recovery, ownership, accounting and cache isolation
  remain covered. Self-spawned tests retain explicit, checked test selectors.

## Lifecycle

- `status`, `doctor`, and help/version do not launch the backend. `service start`
  and `start` explicitly launch it without opening a window.
- Closing or quitting the UI leaves the backend and gateway running. The tray's
  Stop All and Quit action requests confirmation, then shuts down the backend.
- A disconnected UI offers Start backend. It does not silently restart a service
  during an updater operation or replay an earlier mutation.
- Connect on launch belongs to backend startup, not opening/reopening a UI.
  Login startup remains an explicit desktop OS preference.
- Native wake monitoring also belongs to the backend: IOKit on macOS, power
  callbacks on Windows, and login1 on Linux. Recovery does not need an open UI.
- The service acquires its instance lock before loading state.
  Client startup and update replacement share an additional pre-spawn gate.
- Shutdown enters draining before taking the exclusive operation gate, waits
  for existing mutations, restores managed agent configuration, stops listeners,
  and awaits process exit. Failure to restore leaves management available for
  recovery instead of closing the inference listener halfway through shutdown.
- A parent-pipe supervisor owns ACI. Backend death closes the pipe in the kernel;
  the supervisor terminates and reaps its verifier child. Normal stop waits for the
  supervisor; reap timeout preserves the completion handle for a later retry.

## Security and Protocol

Local management uses bounded, versioned NDJSON over Unix sockets or Windows
named pipes. It is separate from the local inference HTTP API. An inference key
never authorizes administration.

Unix endpoints live in a validated private per-user directory and authenticate
peer UID in both directions. Endpoint paths are shortened for Unix socket limits.
Only an instance-lock owner may reclaim a stale socket, and only the socket inode
owned by a listener is removed when it closes.

Windows uses a protected current-user DACL, rejects remote clients, protects the
first pipe instance, and verifies both peer process token SIDs. Read/write
operations use overlapped I/O with timeout cancellation and completion draining.
Same-user malicious code and OS administrators are outside this isolation boundary.

Clients validate the server handshake before sending requests. Shutdown includes
the expected instance ID on that same authenticated connection. Update validation
also checks that the running executable belongs to the current installation.
The service bounds frame sizes, frame deadlines, clients, subscriptions, and
exports. Slow or malformed clients cannot close the service. Subscriptions have
a separate quota so they cannot consume every short-request slot.

Agent changes retain preview/revision/apply validation. CSV exports are streamed
one row at a time into a newly created private file; existing targets and symlinks
are not overwritten. OS credential stores remain the only persistent provider
credential store. UI-free operation does not imply an unlocked credential store
on an unattended machine; there is no plaintext fallback.

## Application identity

The display name is Private AI Proxy, with by dstack TEE attribution.
The application identifier is `org.dstack.private-ai-proxy`; storage and
credential services use the new identity and local keys use `sk-pap-`.
Old beta configuration is not migrated. Command registration installs `private-ai-proxy` and its `pap` shortcut,
and refuses unrelated existing commands.

Windows uses the official Tauri NSIS template with branded artwork and
no legacy-installation branches. Linux packages use the Private AI Proxy name.
Beta and stable remain separate. Independent CLI archives contain the three
console executables and do not require the desktop UI.

## Sources

Repository: https://github.com/Dstack-TEE/private-ai-gateway
Primary contracts: `apps/desktop/cli/args.rs`, `apps/desktop/runtime/src/cli/args.rs`,
`apps/desktop/runtime/src/process.rs`, `apps/desktop/scripts/package-cli.mjs`.

Official contracts: [Rust file locks](https://doc.rust-lang.org/1.89.0/std/fs/struct.File.html#method.try_lock),
[Tauri sidecars](https://v2.tauri.app/develop/sidecar/),
[Tauri NSIS hooks](https://v2.tauri.app/distribute/windows-installer/),
[Windows pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights),
[XDG runtime directories](https://specifications.freedesktop.org/basedir/latest/),
[Secret Service](https://specifications.freedesktop.org/secret-service/latest/ch01.html),
[Apple power notifications](https://developer.apple.com/library/archive/qa/qa1340/_index.html),
[CoreFoundation run-loop sources](https://github.com/apple-oss-distributions/CF/blob/main/CFRunLoop.h),
[Windows power callbacks](https://learn.microsoft.com/en-us/windows/win32/api/powerbase/nf-powerbase-powerregistersuspendresumenotification),
[login1 signals](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html).
