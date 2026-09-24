# Private AI Proxy architecture

Status: implemented.

## Responsibilities

| Component | Responsibility | Implementation |
| --- | --- | --- |
| Private AI Gateway | Remote attested inference service and signed receipts | `src/aggregator`, `src/middleware` |
| Private AI Proxy | Desktop profiles, agent connections, verification and usage | `apps/desktop` |
| Local backend | Sessions, configuration transactions, local API and verifier task ownership | `apps/desktop/runtime`, `apps/desktop/agent-bridge` |
| `private-ai-proxy` | Unified managed CLI and ACI protocol commands | `apps/desktop/cli` |
| `private-ai-proxy-service` | Per-user backend entry point | `apps/desktop/cli/service.rs` |

## Project boundary

`private-ai-proxy` composes the managed CLI's Clap command tree with its ACI
commands. The `cli` crate is the only command-line surface: management
arguments, execution, output and completions live in
`apps/desktop/cli/manage`. The ACI commands and the verifying proxy
(`cli/serve.rs`, which the backend also runs in managed mode) form the crate's
library, built once for both executables. The backend crates contain no
argument parsing or terminal prompting.
There is one PAP user-facing executable and one PAP relying-party verifier.
Desktop integration adds lifecycle events for process integration and post-delivery receipt auditing.
The Private AI Proxy package owns the user-facing CLI, its managed service binary,
and its relying-party ACI modules under `apps/desktop/cli/aci`; shared protocol
encoding lives in `crates/aci-protocol`. The CLI always includes its managed
runtime because every supported build and package ships both executable targets.

Private AI Gateway and Private AI Proxy are independent projects. Gateway owns
its service-side ACI implementation; PAP owns its relying-party verification,
audit, and local-proxy implementation. Neither Rust package imports the other;
both depend on the neutral `aci-protocol` crate for wire types and deterministic
encoding only. Verification policy and security decisions are not shared, and
each project retains its own workspace and lockfile. `pap` is the preferred
shell command; the full `private-ai-proxy` name and the legacy `aci` alias
invoke the same executable rather than separate binaries or crates. `aci`
prints a one-line note toward `pap` only on an interactive terminal outside
JSON modes, so scripted use keeps identical output.

`pap verify/audit/sessions/send` do not initialize the managed backend or
read the settings files. `pap serve` streams responses immediately and audits receipts
afterward by default. Receipt checks never gate streaming. `pap --json serve`
emits lifecycle JSON events.

`pap start/stop` retain managed profiles, user-session continuity and reversible
agent configuration. The backend runs the same verifier implementation as
`private-ai-proxy serve` as an owned Tokio task. The task has no listener of
its own; stopping or losing the backend drops its only request path.
Direct and independent CLI packages contain `private-ai-proxy`,
`private-ai-proxy-service` and the credential helper. MAS contains only the
service. No distribution contains an independent `aci` executable.

## Crate boundaries

| Crate | Package | Responsibility |
| --- | --- | --- |
| `cli` | `private-ai-proxy` | Every command-line surface, the ACI verifier, `pap serve`, and the `private-ai-proxy-service` entry point that injects the verifier into the backend |
| `core` | `private-ai-proxy-core` | Client side shared by every process: renderer contracts, the management API (command table, typed errors) and its HTTP client, the local endpoint, backend launch, shared UI API, release-channel update checks, the `config.toml` model, its validation and JSON Schema, app paths, locks and owner-only file primitives |
| `runtime` | `private-ai-proxy-runtime` | The backend: controller, the settings files (`config.toml`, `credentials.toml`: in-place edits, live reload, the one-time 0.1 import), device-local secrets (`local_state`), the management API server and command dispatch, verifier session state machine (`verifier_session`), usage store, account login, web UI, wake monitoring |
| `agent-bridge` | `private-ai-proxy-agent-bridge` | Loopback Local API proxy, agent tokens, verified catalog, reversible agent configuration, and `private-ai-proxy-helper` |
| `src-tauri` | `private-ai-proxy-desktop` | Tauri shell: windows, tray, menus, notifications, updates |

Dependencies point one way: `src-tauri` → `core`; `agent-bridge` → `core`;
`runtime` → `agent-bridge`, `core`; `cli` → all three. The desktop shell only
talks to the backend over its management API, so it links neither the backend nor the agent
bridge (no HTTP server, SQLite, keyring, agent config editors or CLI parser). The
backend binary lives in `cli` because it injects the in-process verifier, which
`cli` owns, into `runtime` through `VerifierLauncher`.

"Gateway" names the remote Private AI Gateway. The renderer, desktop shell,
web UI and backend ship together (clients require a matching `BUILD_VERSION`, and
web UI sessions end when the service restarts), so their internal contract,
command and event names can change in one release. Only persisted data keeps
older names: the `gateway` notification preference, which covers local
protection problems, is stored under that key.

## Module boundaries

- Renderer `index.tsx` owns bootstrap; `app.tsx` composes the window and its
  dialogs. `features/` contains pages and dialogs, `hooks/` owns reusable
  interactions, and `lib/` contains presentation rules and the live desktop API
  binding. Features never import the app.
- Dialogs are shadcn `Dialog`s in the one window, in the desktop app and the web
  UI alike; decisions use `AlertDialog`. Tray and menu items show the window and
  send `pap://navigate` for the page, dialog or documentation link. Failures show
  inline in their dialog, form or page, and as a toast for other in-window
  actions, including app-menu items. Failed tray actions are only logged, like
  other tray apps; the tray and the window show the state that applies.
- Runtime `controller.rs` owns shared state and launch; its private modules group
  lifecycle, profiles, account login, credentials, agents and local endpoints.
  The same locks and transaction guards span these implementation modules.
- Agent registry, provider data, projection, discovery and validation are separate
  modules. Apply, disconnect, recovery and rollback stay together in transactions.
- OAuth provider/HTTP/billing helpers and verifier events are separate from their
  session owners. Tauri command modules adapt the shared runtime to the API.
- `core/src/ui_api.rs` is the renderer management table. A method is either the
  management command of the same name or composed by a `Host` (tray state,
  Open at Login, notifications) from commands. The names are the Tauri command
  names and the web RPC paths, so the renderer, the CLI and both transports
  use one name per command. The desktop shell runs methods with its own host
  and sends commands to the service; the service answers browsers' methods with
  its own. The renderer builds one `DesktopApi` from a transport and platform
  primitives.
- Core tests are grouped by behavior. Layout, color and asset-name assertions are
  excluded; authorization, recovery, ownership, accounting and cache isolation
  remain covered. Self-spawned tests retain explicit, checked test selectors.

## Lifecycle

- `status`, `doctor`, and help/version do not launch the backend. `service start`
  and `start` explicitly launch it without opening a window.
- Closing or quitting the UI leaves the backend and protection running. The tray's
  Stop All and Quit action requests confirmation, then shuts down the backend.
- A disconnected UI offers Start backend. It does not silently restart a service
  during an updater operation or replay an earlier mutation.
- Connect on launch belongs to backend startup, not opening/reopening a UI.
  Login startup remains an explicit desktop OS preference.
- Native wake monitoring also belongs to the backend: IOKit on macOS, power
  callbacks on Windows, and login1 on Linux. Recovery does not need an open UI.
- The service acquires its instance lock before loading state; a second
  backend exits, and a client that started it waits for the one that won.
- `startup.lock` is the installer gate, a reader-writer file lock: clients
  starting the backend hold it shared, so they never wait for each other;
  an installer or updater holds it exclusively while it stops the backend and
  replaces files, and clients report it after 5 s instead of waiting.
- Shutdown enters draining before taking the exclusive operation gate, waits
  for existing mutations, restores managed agent configuration, stops listeners,
  and awaits process exit. Failure to restore leaves management available for
  recovery instead of closing the inference listener halfway through shutdown.
- An owned verifier task performs attestation, TLS pinning and receipt audits in
  the backend process. Task failure publishes the error/reconnect state;
  explicit stop cancels the task after first revoking the published forwarding
  session.

## Verifier execution

Managed inference has one local HTTP listener. After authenticating the Agent,
the Local API proxy swaps in the provider credential and calls the verified service
directly with the request body and typed attribution context. The verifier makes
the sole remote HTTP hop over its attestation-bound TLS client. Lifecycle events
use a direct callback, and request delivery, usage and receipt-audit updates all
flow through `ProxyEvent`.

The verifier runs as a task owned by the backend, so it shares the backend's
lifetime and has no separately reachable port. Standalone `pap serve` uses the
same verifier with its own proxy listener and the documented `--control`
receipt-audit listener; managed mode binds neither and calls the verifier
directly.

## Security and Protocol

Local management is one HTTP API (`core/src/protocol.rs`) the service serves on
its private endpoint and, for the web UI, on TCP, as Docker Engine serves one
API on `unix://` and `tcp://`: `GET /api/version`, `POST /api/rpc/{command}`
with the command's parameters as a JSON object, and `GET /api/events`, a
server-sent event stream that starts with a state snapshot. Errors are
`{"error": {"code", "message"}}` with a stable code and the HTTP status of its
Docker `errdefs` class. It is separate from the local inference HTTP API. An
inference key never authorizes administration.

The local endpoint is a Unix socket or a Windows named pipe; peers are
authenticated by their OS user, as Tailscale's LocalAPI authenticates its Unix
socket peers. Unix endpoints live in a validated private per-user directory,
are `0600`, and check the peer UID in both directions (`SO_PEERCRED` or
`getpeereid`). Endpoint paths are shortened for Unix socket limits. Only an
instance-lock owner may reclaim a stale socket, and only the socket inode
owned by a listener is removed when it closes.

The MAS service keeps its management socket in the short `pap-ipc` directory at
the root of its own App Container. It does not fall back to `/private/tmp`, which
is outside the sandbox's writable container boundary.

The service also resolves the persisted Home bookmark before Agent work and holds
the resulting security-scoped access for that work; a status check never grants
only a temporary scope that is dropped before the scan.

Windows uses a protected current-user DACL, rejects remote clients, protects the
first pipe instance, and verifies both peer process token SIDs.
Same-user malicious code and OS administrators are outside this isolation boundary.

Clients check `GET /api/version` on each connection before calling and refuse
another build. Shutdown names the expected instance ID. Update validation also
checks that the running executable belongs to the current installation. A
browser session may run only the renderer's methods; the shutdown, export and
maintenance commands are the local owner's. The service bounds response and
event sizes and exports; accept errors such as `EMFILE` back off for a second
instead of stopping the service, and malformed or disconnected clients close
only their own connection.

The optional web UI is a second, browser-facing transport owned by the service.
It is off by default and binds `127.0.0.1` unless network access is explicitly
allowed; its listener settings share `ListenConfig` and `listen::resolve` with
the Local API, so non-loopback addresses fail closed without confirmation.
Setting changes apply live and bind failures are reported in state. It requires
a sign-in password, stored only as an Argon2id hash in `credentials.toml` and set
over the local endpoint (its root of trust) or by a signed-in browser that proves
the current password. Signing in sets an `HttpOnly`, `SameSite=Strict` cookie
for an idle-expiring server-side session; mutations also need an exact
`Origin`.
Changing the password, disabling the web UI, moving its listener, resetting
settings or restarting the service revokes every session. Requests require an
allowed `Host` (the bound address, the client host, loopback when bound to
every interface, or `localhost` when loopback reaches the listener), origin
headers for that host (always present on `POST`) and JSON mutations, then run
through the same router, admission and dispatch as the local endpoint. Sign-in attempts and rejected requests share a token bucket. Its state
stream is fed from the controller's state channel. Mac App Store builds omit it.

Agent changes retain preview/revision/apply validation. CSV exports are streamed
one row at a time into a newly created private file; existing targets and symlinks
are not overwritten. Cells a spreadsheet would read as a formula get a leading
`'` (OWASP CSV injection). The usage database is versioned by
`PRAGMA user_version` through `rusqlite_migration`; a database written by a newer
release is refused, not rewritten, and usage stays in memory for that run. Provider credentials persist only in the owner-only
`credentials.toml` ([Settings files](configuration.md)), so UI-free operation
on an unattended machine needs no unlocked OS keychain.

## Application identity

The display name is Private AI Proxy, with by dstack TEE attribution.
The application identifier is `org.dstack.private-ai-proxy`; storage and
credential services use the new identity and local keys use `sk-pap-`.
Old beta configuration is not migrated. Command registration installs `pap`,
`private-ai-proxy`, and `aci`, and refuses unrelated existing commands.

Windows uses the official Tauri NSIS template with branded artwork and
no legacy-installation branches. Linux packages use the Private AI Proxy name.
Beta and stable remain separate. Independent CLI archives contain the three
console executables and do not require the desktop UI.

## Sources

Repository: https://github.com/Dstack-TEE/private-ai-gateway
Primary contracts: `apps/desktop/cli/args.rs`, `apps/desktop/cli/manage/args.rs`,
`apps/desktop/core/src/protocol.rs`, `apps/desktop/cli/serve.rs`,
`apps/desktop/runtime/src/verifier_session.rs`,
`apps/desktop/scripts/package-cli.mjs`.

Official contracts: [Rust file locks](https://doc.rust-lang.org/1.89.0/std/fs/struct.File.html#method.try_lock),
[Tauri NSIS hooks](https://v2.tauri.app/distribute/windows-installer/),
[Windows pipe security](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights),
[XDG runtime directories](https://specifications.freedesktop.org/basedir/latest/),
[Secret Service](https://specifications.freedesktop.org/secret-service/latest/ch01.html),
[Apple power notifications](https://developer.apple.com/library/archive/qa/qa1340/_index.html),
[CoreFoundation run-loop sources](https://github.com/apple-oss-distributions/CF/blob/main/CFRunLoop.h),
[Windows power callbacks](https://learn.microsoft.com/en-us/windows/win32/api/powerbase/nf-powerbase-powerregistersuspendresumenotification),
[login1 signals](https://www.freedesktop.org/software/systemd/man/latest/org.freedesktop.login1.html).
