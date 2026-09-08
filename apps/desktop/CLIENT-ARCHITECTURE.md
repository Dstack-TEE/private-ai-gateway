# Private AI Proxy architecture

Status: implemented on the desktop feature branch, 2026-09-08.

## Responsibilities

| Component | Responsibility | Implementation |
| --- | --- | --- |
| Private AI Gateway | Remote attested inference service and signed receipts | `src/aggregator`, `src/middleware` |
| Private AI Proxy | Desktop profiles, agent connections, verification and usage | `apps/desktop/src/renderer` |
| Local backend | Sessions, configuration transactions, local API and process ownership | `apps/desktop/runtime`, `apps/desktop/gateway` |
| `pap` | Unified managed-client and ACI protocol commands | `src/bin/pap` |
| `pag` | Legacy management command | Thin wrapper over `desktop_runtime::cli` |
| `aci` | Existing standalone protocol reference CLI, unchanged and not bundled | `src/bin/aci` |

## Shared implementation

`pap` composes the managed CLI's Clap command tree with the existing ACI
commands. Management command execution and output live in
`apps/desktop/runtime/src/cli`, shared with `pag`. ACI modules are compiled from
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
Packages contain `pap`, the legacy `pag`, `pag-service` and the credential
helper. They do not contain an independent `aci` executable.

## Upgrade compatibility

The display name is Private AI Proxy, with by dstack TEE attribution.
Application identifiers, credential service keys, backend protocol, local-token
prefix, storage paths and update signing keys stay unchanged. Default command
registration now registers `pap` and refuses unrelated existing commands.
The bundled `pag` retains existing management scripts.

Windows uses the official Tauri NSIS template with branded artwork and an
explicit legacy-installation check. Linux desktop packages declare replacement
of the old product package. Beta and stable remain separate; a rename does not
authorize stable promotion.

## Sources

Repository: https://github.com/Dstack-TEE/private-ai-gateway
Main integrated before this change: `c2d31a8`.
Primary contracts: `src/bin/aci/args.rs`, `apps/desktop/runtime/src/cli/args.rs`,
`apps/desktop/runtime/src/process.rs`, `apps/desktop/scripts/package-cli.mjs`.
