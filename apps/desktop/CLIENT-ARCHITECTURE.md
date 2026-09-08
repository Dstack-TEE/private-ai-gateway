# Client naming and CLI boundaries

Status: proposal, 2026-09-08. This document does not rename installed apps,
commands, credential services, application identifiers, or update channels.

## Responsibilities

| Component | Responsibility | Implementation |
| --- | --- | --- |
| Remote Private AI Gateway | Attested inference service, routing, provider evidence, signed receipts | `src/aggregator`, `src/middleware` |
| Desktop client | Profiles, agent connections, local verification status and usage | `apps/desktop/src/renderer` |
| Local backend | Owns session lifecycle, agent configuration transactions and local API | `apps/desktop/runtime`, `apps/desktop/gateway` |
| `pag` | Human and machine interface to the local backend | `apps/desktop/runtime/src/bin/pag` |
| `aci` | Protocol verification, offline audit, sessions, individual requests and verifying proxy | `src/bin/aci` |

The local app is an ACI client and verifying proxy, not the remote inference
gateway. It also manages state and agent configuration, so "proxy" alone does
not describe the whole product. The backend already runs the bundled `aci serve`
with strict receipt verification. CLI packages already contain both binaries;
this is shared distribution, not a unified command interface.

## Naming recommendation

Use **Private AI Client** for the user-facing app, with **by dstack TEE** as the
existing attribution. Describe its Local API as a **local verifying proxy**.
Keep **Private AI Gateway** for the remote service. "ACI Client" is precise for
developer documentation but does not explain the product to a new user.

Approve the public name before changing it. A display-name change must preserve
the application identifier, data paths, credential-store service identifiers,
update signing keys and feed compatibility. Keep the `pag` command as a
compatibility entry point even if a new command name is introduced.

## CLI recommendation

Unify the user entry point while retaining a reusable ACI implementation:

- `pag verify`, `pag audit`, `pag sessions` and `pag send` can expose the existing
  protocol operations without starting the managed backend or accessing its
  credentials by default.
- Keep `pag start/stop`, profiles, agents, settings and usage as managed-client
  operations with their existing consent and JSON contracts.
- Keep `aci serve` as the standalone verifier/proxy interface. It must not be
  silently replaced by `pag start`: listener ownership, credentials, lifetime,
  configuration projection and receipt policies differ.
- Extract ACI command execution from its binary entry point into a shared Rust
  library. Keep thin Clap frontends for both command names. Do not copy verifier
  logic into the desktop crate or parse human-readable subprocess output.
- Preserve existing `aci` flags, exit codes and JSON output for scripts. New
  client commands must retain `pag`'s non-interactive and machine-output rules;
  define adapters explicitly where the existing output schemas differ.

Implement this as a separate compatibility change after the naming decision,
with contract tests for the existing `aci` interface and no mandatory GUI,
daemon or keychain dependency for offline audit.

## Source snapshot

Reviewed client baseline: `c03eec2ee148aba4e2a4a0e145432675d67aae54`.
Main integrated into the feature branch: `c2d31a8` (2026-09-08 fetch).
Repository: https://github.com/Dstack-TEE/private-ai-gateway
Sources: `src/bin/aci/args.rs`, `apps/desktop/runtime/src/bin/pag/args.rs`,
`apps/desktop/runtime/src/gateway.rs`, `apps/desktop/scripts/package-cli.mjs`.
