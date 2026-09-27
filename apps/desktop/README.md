# Private AI Proxy

Private AI Proxy is the local client for confidential AI services. It
verifies an ACI service, exposes a machine-local API, and projects that API into
supported coding agents without giving those agents the provider credential.
It ships as a desktop app and the `pap` CLI, which both manage one per-user
backend service. This directory holds all of them and their packaging.

Private AI Proxy and the remote Private AI Gateway are independent projects that
share only neutral protocol crates; [Architecture](docs/client-architecture.md#project-boundary)
describes the boundary.

## Documentation

- [Install](../../docs/private-ai-proxy-install.md)
- [Architecture](docs/client-architecture.md)
- [CLI reference](docs/cli.md)
- [Settings files](docs/configuration.md)
- [CLI distribution](docs/cli-distribution.md)
- [Account login](docs/account-login.md)
- [Distribution architecture](docs/distribution.md)
- [Mac App Store preparation](docs/mac-app-store.md)
- [Codex baseline and catalog refresh](docs/codex-catalog.md)
- [ACI specification](../../spec/aci.md)

## Repository Layout

| Path | Responsibility |
| --- | --- |
| `src/renderer` | React UI and native-window content |
| `src-tauri` | Tauri application, system integration, tray, menus, dialogs, updates |
| `cli` | The `pap` CLI, the only command-line surface (arguments, output, completions), ACI relying-party verifier, local streaming proxy, and the `private-ai-proxy-service` entry point |
| `core` | Client side shared by the app, CLI and backend: contracts, the management API and its client, the local endpoint, the `config.toml` model, paths |
| `runtime` | Persistent backend: controller, settings files, the management API server, verifier sessions, usage, account login, web UI |
| `agent-bridge` | Coding-agent bridge: Local API proxy, agent tokens, catalog, reversible agent configuration, and the `private-ai-proxy-helper` binary |
| `gateway/src/endpoint-support.json` | Published model endpoint inventory; released apps fetch this path, so it stays put |
| `../../crates/aci-protocol` | Shared ACI wire types and deterministic encoding rules |
| `../../crates/aci-verify` | Shared policy-neutral ACI verification mechanisms: report binding, dstack event-log replay and KMS custody chain, declared TLS selection |
| `brand` | Source branding and icon assets |
| `npm` | npm packaging of the CLI |
| `scripts` | Reproducible build, packaging, release, and endpoint-probe tooling |

Every package ships the same three sibling executables; see
[CLI distribution](docs/cli-distribution.md).

## Request Path

```text
coding agent
    -> Local API with an agent-scoped token
    -> in-process verifier over a verified ACI channel
    -> confidential AI service
```

Identity, policy, credential ownership, and model admission gate request
delivery. Response bytes stream immediately. Signed receipts are fetched and
audited afterward; an audit failure updates Usage but cannot retract bytes that
were already delivered. Usage keeps each checked receipt, shown in the request's
proof details and printed by [`pap usage show <id> --receipt`](docs/cli.md#coverage).

Only agents that are both linked and currently protected are authorized on the
Local API. Stopping protection, Stop All and Quit, `pap service stop`, Reset
settings, or disconnecting restores the owned configuration; quitting the app
from the tray or menu leaves the backend running and the agents pointed at it.
A verification failure or block, a network loss, and a restart for an update,
a profile switch or a settings change keep linked agents pointed at the Local
API, which refuses them until protection is verified again, so their requests
never fall back to the original provider. A Local API address change during
the session rewrites them to the new address from the last verified catalog;
only when this backend has not verified a catalog yet are they restored until
verification projects them again. If neither the new address nor the old
one can be bound, agents stay pointed at an address with no listener, which
refuses them. External edits are preserved and incomplete restoration is kept
retryable.

A backend that is not running restores nothing: `pap service stop` and
uninstalling cannot restore agents it left projected (for example after an
update installed without relaunching the app). Run `pap service start` and
then `pap service stop`, or choose Stop All and Quit in the app, before
uninstalling.

## Development

Requirements:

- Node.js 22.19 or newer
- npm 11
- Current Rust stable toolchain (1.98 or newer)
- Tauri platform dependencies for the host OS
- Xcode 26 or newer when generating the adaptive macOS icon

Install dependencies and run the desktop app:

```bash
cd apps/desktop
npm ci
npm run dev
```

Build a local package:

```bash
npm run dist
```

Build only the `pap` CLI:

```bash
cd apps/desktop
cargo build --package private-ai-proxy --bin private-ai-proxy
```

All five Rust packages under `apps/desktop` share this workspace's `Cargo.lock`
and write build output to `apps/desktop/target` unless `CARGO_TARGET_DIR` is set.
Tauri, its sidecars, and the standalone CLI therefore resolve one dependency
graph and reuse one build cache.

The Tauri development server is only the renderer transport used by
`tauri dev`. The same renderer, built with `npm run build:web`, is the web UI
the backend service serves (see [the CLI guide](docs/cli.md#web-ui)); it talks
to the real service, never to a mock desktop runtime, and there are no
screenshot-specific production branches.

## Verification

Run the focused checks from `apps/desktop`:

```bash
npm run check
npm run test:release
npm run test:probe

cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Renderer DTOs in `src/shared/contracts.generated.ts` are generated from the
Rust contracts with `npm run generate:contracts`; `cargo test` fails when the
committed file is stale.

The Rust suites cover protocol verification, local proxy behavior, lifecycle,
configuration transactions, recovery, usage, and native command boundaries.
Renderer correctness is checked by TypeScript and exercised through the real
Tauri application; there is no in-page mock API or Playwright screenshot suite.

## Profiles And Credentials

Profiles contain a name, provider, service endpoint, and authentication method.
[Settings files](docs/configuration.md) describes where profiles and their
credentials are stored, and [Account login](docs/account-login.md) covers the
Phala and RedPill sign-in flows. The renderer, `config.toml`, agent
configuration, command arguments, and diagnostics never receive the raw saved
credential.

## Model Compatibility Inventory

`gateway/src/endpoint-support.json` contains dated endpoint observations for the
RedPill and Phala presets. The verified live catalog remains authoritative;
inventory data only filters models that are known to match an agent's API
surface.

Probe a service explicitly:

```bash
node scripts/probe-model-endpoints.mjs \
  --endpoint https://tee.redpill.ai \
  --key-env REDPILL_AI_API_KEY \
  --previous gateway/src/endpoint-support.json \
  --json
```

Pass credentials through the named environment variable, never as command-line
arguments. Temporary HTTP failures remain availability failures rather than
proof of incompatibility. Network errors and malformed responses remain
inconclusive. Review the complete report before updating the inventory.

## Branding

Product identity is committed where each consumer reads it:

- `src-tauri/tauri.conf.json` is the source of the product name (also the main
  window title), identifier, deep-link scheme, publisher, bundle metadata,
  installer images and the DMG layout. The Linux packages
  (`scripts/package-linux.mjs`) and the renderer (`src/renderer/brand/brand.ts`
  imports `productName`) read it directly. `core/src/brand.rs` repeats the
  product name, identifier and publisher for Rust, and its unit test fails when
  they differ from the config. The identifier names the data directory and
  credential namespace, so a mismatch would split user data.
- `core/src/brand.rs` also holds the byline and the support link, and derives
  the default service from `ServiceProvider::DEFAULT`; the renderer receives
  the byline and the providers as generated contracts.
- `package.json` `bugs.email`: the support contact, used as the Linux package
  maintainer address.
- Images: `src-tauri/icons/` (from `tauri icon`, plus the Icon Composer project
  `AppIcon.icon` that `npm run build` compiles on macOS), the tray icons
  `assets/tray/{protected,unprotected}{,-dark}.png` (the black ones are the
  macOS menu bar template), `src/renderer/brand/app-icon-{light,dark}.png`,
  and the installer images
  `src-tauri/installer/brand-header.bmp` (150×57), `brand-sidebar.bmp`
  (164×314, the NSIS sizes) and `brand-dmg-background.png` (660×440, matching
  `bundle.macOS.dmg`).

Source artwork and its provenance are in [`brand/dstack`](brand/dstack/README.md).
Regenerate the desktop icons with Tauri's icon command, then commit the files
that `bundle.icon` lists:

```bash
npm exec tauri icon brand/dstack/icon/app-icon.png -- -o /tmp/icons
```

A second brand replaces exactly these values and images. It can do this in a
`tauri build --config` overlay for the Tauri values, its own `brand.rs` and
`brand.ts`, and its support contact. Its identifier gives it a separate data and credential
namespace. The default identifier `org.dstack.private-ai-proxy` also differs
from earlier beta builds.

## Packaging And Releases

`.github/workflows/desktop-native.yml` builds the desktop app installers, the
Linux packages and the portable CLI archives.
[CLI distribution](docs/cli-distribution.md) defines what each package contains
and the release contract: tags, assets, signing and channels.
[Distribution architecture](docs/distribution.md) covers release orchestration
and how each installation updates.

## Design Rules

- Keep policy, persistence, verification, and lifecycle in Rust.
- Keep renderer components presentation-only and use Tauri for OS integration.
- Prefer one implementation and one source of truth over compatibility layers.
- Treat profiles, credentials, agent configuration, and update installation as
  explicit transactions with recoverable failure states.
- Do not add browser-only runtime branches to production code.
- Do not duplicate Gateway producer logic in the Proxy verifier.
- Preserve user-owned configuration and fail closed when ownership is ambiguous.
