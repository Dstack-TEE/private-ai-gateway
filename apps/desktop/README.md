# Private AI Proxy

Private AI Proxy is the local desktop client for confidential AI services. It
verifies an ACI service, exposes a machine-local API, and projects that API into
supported coding agents without giving those agents the provider credential.

The desktop product and the remote Private AI Gateway are independent projects:

- Gateway owns remote inference, service-side attestation, sessions, and signed
  receipt production.
- Proxy owns local profiles, relying-party verification, post-delivery receipt
  audits, agent configuration, usage history, and desktop lifecycle.
- They share only the neutral `aci-protocol` wire types and canonical encoding
  crate. Producer logic and relying-party verification remain independent.

The preferred user-facing command is `pap`; `private-ai-proxy` is the
canonical executable name. `aci` is a legacy alias kept for existing scripts:
it runs the same executable and prints a one-line deprecation note only on an
interactive terminal outside JSON modes.

## Documentation

- [Architecture](docs/client-architecture.md)
- [CLI](docs/cli.md)
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
| `cli` | The only command-line surface (arguments, output, completions), ACI relying-party verifier, local streaming proxy, and the `private-ai-proxy-service` entry point |
| `core` | Client side shared by the app, CLI and backend: contracts, management protocol and client, IPC transport, the `config.toml` model, paths |
| `runtime` | Persistent backend: controller, settings files, management server, verifier sessions, usage, account login, web UI |
| `agent-bridge` | Coding-agent bridge: Local API proxy, agent tokens, catalog, reversible agent configuration, and the `private-ai-proxy-helper` binary |
| `gateway/src/endpoint-support.json` | Published model endpoint inventory; released apps fetch this path, so it stays put |
| `../../crates/aci-protocol` | Shared ACI wire types and deterministic encoding rules |
| `brand` | Source branding and icon assets |
| `scripts` | Reproducible build, packaging, release, and endpoint-probe tooling |

The direct-download application contains three sibling executables:

- `private-ai-proxy`
- `private-ai-proxy-service`
- `private-ai-proxy-helper`

There is no standalone `aci` executable; it is a legacy alias. The package neither embeds nor imports
the remote Private AI Gateway server.

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
were already delivered.

Only agents that are both linked and currently protected receive the local API
configuration. Stop, shutdown, verification failure, or disconnect restores the
owned configuration. External edits are preserved and incomplete restoration is
kept retryable.

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

Build only the unified CLI:

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
Settings live in `config.toml` and provider credentials only in the owner-only
`credentials.toml` beside it; see [Settings files](docs/configuration.md). The
renderer, `config.toml`, agent configuration, command arguments, and diagnostics
never receive the raw saved credential.

Phala and RedPill support account login or manual API keys. Custom ACI services
use manual keys. Saving a profile does not claim that the endpoint is verified;
starting protection performs fresh verification. See
[Account login](docs/account-login.md) for provider-specific flows.

The local client key uses `sk-pap-` followed by 64 lowercase hexadecimal
characters. It authenticates local inference, not backend administration.

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

- `src-tauri/tauri.conf.json`: product name, identifier, deep-link scheme,
  bundle metadata, installer images and the DMG layout. The Linux packages
  (`scripts/package-linux.mjs`) take their names and descriptions from it too.
- `core/src/brand.rs` for Rust and `src/renderer/brand/brand.ts` for the
  renderer: names, links, service defaults and theme colors.
- Images: `src-tauri/icons/` (from `tauri icon`, plus the Icon Composer project
  `AppIcon.icon` that `npm run build` compiles on macOS), `assets/tray/trayTemplate@2x.png`,
  `src/renderer/brand/app-icon-{light,dark}.png`, and the installer images
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
`tauri build --config` overlay for the Tauri values, plus its own `brand.rs`
and `brand.ts`. Its identifier gives it a separate data and credential
namespace. The default identifier `org.dstack.private-ai-proxy` also differs
from earlier beta builds.

## Packaging And Releases

The desktop workflow builds separate macOS arm64/x64 DMGs, Windows NSIS
installers, and Linux DEB/RPM/Arch packages, plus portable CLI archives. AppImage is
not supported because its transient mount is incompatible with a persistent
per-user backend.

Release and updater behavior is documented in
[CLI distribution](docs/cli-distribution.md) and implemented by
`.github/workflows/desktop-native.yml`. Published updates use Tauri's signed
updater artifacts. macOS distribution additionally requires Developer ID
signing and notarization. Windows Authenticode signing is optional; when its
certificate is configured, CI imports it only for the package job, configures
Tauri with its thumbprint, signs the bundled executables before packaging,
verifies the installer and installed executables, and removes it afterward.

Beta and stable are update channels; stable releases are also published to the
beta feed. A release version, channel, published assets, and feed metadata must
agree. Published releases must run from `main` and include all supported
platforms. The tag, title, notes, asset naming, signing credentials,
and channel rules are defined in [CLI distribution](docs/cli-distribution.md).

## Design Rules

- Keep policy, persistence, verification, and lifecycle in Rust.
- Keep renderer components presentation-only and use Tauri for OS integration.
- Prefer one implementation and one source of truth over compatibility layers.
- Treat profiles, credentials, agent configuration, and update installation as
  explicit transactions with recoverable failure states.
- Do not add browser-only runtime branches to production code.
- Do not duplicate Gateway producer logic in the Proxy verifier.
- Preserve user-owned configuration and fail closed when ownership is ambiguous.
