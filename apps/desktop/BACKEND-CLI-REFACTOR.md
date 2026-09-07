# PAG Backend and CLI

The desktop UI and `pag` connect to one per-user `pag-service` backend. Only the
backend owns gateway state, profile credentials, agent projection, usage,
preferences, and the ACI subprocess. Neither client opens an inference listener
or writes backend configuration directly.

```text
React -> Tauri commands -> Rust Client --+
                                       +-> private IPC -> pag-service -> aci
pag --------------------> Rust Client --+
```

This is stacked on desktop PR #177. It retains the shared renderer and existing
gateway/verifier implementation; it does not restore the older experimental
native clients or their stdio runtime.

## Commands

```sh
pag status --json
pag status --watch --json
pag service start
pag start --profile work
pag stop
pag service stop --yes
pag profiles add --id work --name Work --url https://tee.redpill.ai --provider redpill
pag profiles verify work
pag profiles list
pag profiles import profiles.json --yes
pag profiles export --output profiles.json
pag profiles use work --yes
pag agents list
pag agents connect codex --model MODEL --dry-run
pag agents connect codex --model MODEL --yes
pag agents disconnect codex --yes
pag models list --refresh
pag usage list --limit 20
pag usage export --format csv --output usage.csv
pag settings show
pag settings set appearance dark --yes
pag token rotate --yes
pag cli status --json
pag cli install
pag app open
pag doctor
pag diagnostics --output diagnostics.json
```

`--help` lists the complete command and option set. Credentials use a hidden
prompt or explicit `--key-stdin`, never command-line key values. API profile
credentials are not returned. `token show --yes` is the explicit operation that
reveals the local inference token; rotation does not print its value.

Destructive operations require interactive confirmation or `--yes`.
Noninteractive callers receive an error instead of a hanging prompt. `--json`
produces machine-readable results; watch output is NDJSON. Errors go to stderr
with a nonzero exit status. No mutation is retried after a lost response.

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
- The service acquires its instance lock before loading or migrating state.
  Client startup and update replacement share an additional pre-spawn gate.
- Shutdown enters draining before taking the exclusive operation gate, waits
  for existing mutations, restores managed agent configuration, stops listeners,
  and awaits process exit. Failure to restore leaves management available for
  recovery instead of closing the inference listener halfway through shutdown.
- A parent-pipe supervisor owns ACI. Backend death closes the pipe in the kernel;
  the supervisor terminates and reaps its ACI child. Normal stop waits for the
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

## Distribution and Verification

See [CLI distribution](CLI-DISTRIBUTION.md) for package layouts, PATH ownership,
automatic app-start registration, and the stable-path requirement that excludes
AppImage from this backend model. CLI-only distributions contain no UI.

Verification uses the existing gateway/runtime suites, a small set of real-binary
CLI lifecycle tests, native transport tests, package smoke tests, and renderer
interaction tests. No provider credentials, paid inference, or production state
are required. Native CI must run the packaged binaries on each OS; cross-type
checking and renderer mocks are supplementary, not installed-app proof.

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
