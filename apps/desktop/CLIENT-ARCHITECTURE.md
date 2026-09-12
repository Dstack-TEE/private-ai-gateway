# Private AI Proxy architecture

Status: implemented; modular layout updated 2026-09-12.

## Responsibilities

| Component | Responsibility | Implementation |
| --- | --- | --- |
| Private AI Gateway | Remote attested inference service and signed receipts | `src/aggregator`, `src/middleware` |
| Private AI Proxy | Desktop profiles, agent connections, verification and usage | `apps/desktop/src/renderer` |
| Local backend | Sessions, configuration transactions, local API and process ownership | `apps/desktop/runtime`, `apps/desktop/gateway` |
| `pap` | Unified managed-client and ACI protocol commands | `src/bin/pap` |
| `aci` | Existing standalone protocol reference CLI, unchanged and not bundled | `src/bin/aci` |

## Shared implementation

`pap` composes the managed CLI's Clap command tree with the existing ACI
commands. Management command execution and output live in
`apps/desktop/runtime/src/cli`. ACI modules are compiled from
their existing source paths: no verifier implementation is copied or modified.
The explicit `desktop-client` Cargo feature keeps desktop dependencies out of
ordinary service and standalone ACI builds.

`pap verify/audit/sessions/send` do not initialize the managed backend or
credential store. `pap serve` runs the standalone local verifier and always
enforces receipt verification before response delivery. `pap --json serve`
emits lifecycle JSON events. The original `aci` interface stays unchanged.

`pap start/stop` retain managed profiles, user-session continuity and reversible
agent configuration. The backend's supervised verifier process now runs
`pap serve`, replacing the old ACI executable. Ownership-pipe and child-reaping
behavior is preserved; a backend crash must not leave a verifier listening.
Packages contain `pap`, `pap-service` and the credential
helper. They do not contain an independent `aci` executable.

## Module boundaries

- Renderer `index.tsx` owns bootstrap; `app.tsx` composes the window. `features/`
  contains pages and forms, `windows/` adapts them to native windows, `hooks/`
  owns reusable interactions, and `lib/` contains presentation rules and the
  single live/preview API selection. Features never import the app or windows.
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

## Application identity

The display name is Private AI Proxy, with by dstack TEE attribution.
The application identifier is `org.dstack.private-ai-proxy`; storage and
credential services use the new identity and local keys use `sk-pap-`.
Old beta configuration is not migrated. Command registration installs only
`pap` and refuses unrelated existing commands.

Windows uses the official Tauri NSIS template with branded artwork and
no legacy-installation branches. Linux packages use the Private AI Proxy name.
Beta and stable remain separate. Independent CLI archives contain the three
console executables and do not require the desktop UI.

## Sources

Repository: https://github.com/Dstack-TEE/private-ai-gateway
Main integrated before this change: `c2d31a8`.
Primary contracts: `src/bin/aci/args.rs`, `apps/desktop/runtime/src/cli/args.rs`,
`apps/desktop/runtime/src/process.rs`, `apps/desktop/scripts/package-cli.mjs`.
