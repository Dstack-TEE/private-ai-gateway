# Native Desktop Architecture

Status: implementation in progress, September 3, 2026.

## Boundary

Private AI Gateway has one Rust runtime and four presentation adapters during
the migration:

- macOS: SwiftUI with AppKit for menu-bar, window, commands, and lifecycle.
- Windows: WinUI 3 with Windows App SDK for window, tray, dialogs, and startup.
- Linux: GTK4 with libadwaita for window, status icon integration, dialogs, and
  desktop startup.
- Tauri: the existing React client remains available until all native clients
  pass the same parity contract.

The platform clients do not implement gateway policy, persistence, profile
validation, agent projection, usage aggregation, or credential handling. They
launch `private-ai-gateway-desktop-service` as a child process and exchange
versioned newline-delimited JSON over stdin/stdout. The runtime owns the child
ACI verifier, stable Local API listener, OS credential store, SQLite usage
history, agent configuration projection, and the primary-instance lock.

No loopback management server is opened. Standard input/output keeps the
control plane private to the process tree and avoids another credential or
firewall surface. A malformed, oversized, or unsupported message fails closed.

## Protocol

Every line is one UTF-8 JSON value and is limited to 1 MiB.

Request:

```json
{"schemaVersion":1,"id":"42","method":"getState","params":null}
```

Response:

```json
{"schemaVersion":1,"id":"42","result":{}}
```

Failure:

```json
{"schemaVersion":1,"id":"42","error":{"code":"invalid_request","message":"..."}}
```

Event:

```json
{"schemaVersion":1,"event":"stateChanged","payload":{}}
```

Request IDs are client-generated printable ASCII and responses are correlated
only by ID. Events have no request ID. Unknown schema versions and methods are
rejected explicitly. The service serializes writes through one output task so
responses and events cannot interleave at the byte level.

Profile API keys never appear in state, events, logs, configuration files, or
native view models. They enter only in `verifyConfiguration` and are stored in
the OS credential store after verification succeeds. The Local API client key
is intentionally retrievable because the user must configure local tools with
it; it is never included in broadcast state.

## Lifecycle

Normal launch shows the main window immediately. Login launch starts hidden and
keeps the tray/menu-bar item active. Closing the main window hides it; Quit asks
the runtime to shut down, stops the ACI verifier and Local API listener, waits
for child exit, and then terminates the UI process.

The GUI owns the runtime process. Unexpected GUI termination closes stdin; the
service stops protection and exits. Unexpected runtime termination is shown as
a blocking error with Restart Runtime. Only one runtime may hold the existing
per-user instance lock and Local API port. A second UI instance activates the
first platform window through the platform single-instance mechanism.

## Packaging

Each package contains three executables next to one another:

- native platform GUI
- `private-ai-gateway-desktop-service`
- `aci`
- `private-ai-gateway-helper`

Brand assets are generated from `brand/<id>/brand.json` and copied into the
native platform asset catalog at build time. Runtime assets are local; no image,
font, script, or stylesheet is loaded from the network.

## Parity Contract

All four clients must expose the same product behavior:

- Overview with protection state, active profile, Local API copy actions, five
  agents, current-session totals, and five clickable proof rows.
- Profiles list plus separate New/Edit profile dialog; zero profiles opens New
  Profile directly; Verify and Save never starts protection.
- Production OS required by default; Allow dev OS is opt-in and produces a
  visible yellow Dev mode state in the window and tray.
- Agents: Codex, Claude Code, OpenCode, Pi, and Hermes, with direct reversible
  connect/disconnect and verified catalog discovery.
- Persistent Usage with agent/model/time filters, chart, summaries, cursor
  pagination, CSV export, proof details, and explicit Clear History.
- Local API settings, client-key reveal/copy/rotate, network-listener safety,
  endpoint rebind rollback, and connected-agent guard.
- Privacy Verification shows identity, source, channel, policy, checks, and
  receipt explanation without collapsed technical details.
- Native tray/menu, startup integration, file picker, clipboard, notifications,
  accessibility, keyboard navigation, reduced motion, light/dark/high contrast,
  and platform-standard confirmation dialogs.

The native clients are release-ready only after their dedicated CI runner
compiles, packages, launches, and smoke-tests the real runtime protocol. Source
that has not passed its platform compiler is not counted as complete.
