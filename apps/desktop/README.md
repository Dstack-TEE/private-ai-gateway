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

The user-facing command is `private-ai-proxy`. `pap` and `aci` are aliases to
that same executable, not separate binaries or crates.

## Documentation

- [Architecture](docs/client-architecture.md)
- [CLI](docs/cli.md)
- [CLI distribution](docs/cli-distribution.md)
- [Account login](docs/account-login.md)
- [ACI specification](../../spec/aci.md)

## Repository Layout

| Path | Responsibility |
| --- | --- |
| `src/renderer` | React UI and native-window content |
| `src-tauri` | Tauri application, system integration, tray, menus, dialogs, updates |
| `runtime` | Persistent backend, profiles, sessions, IPC, lifecycle, usage |
| `gateway` | Local inference API and reversible agent configuration |
| `cli` | Unified CLI, ACI relying-party verifier, and local streaming proxy |
| `../../crates/aci-protocol` | Shared ACI wire types and deterministic encoding rules |
| `brand` | Source branding and icon assets |
| `scripts` | Reproducible build, packaging, release, and endpoint-probe tooling |

The packaged application contains three sibling executables:

- `private-ai-proxy`
- `private-ai-proxy-service`
- `private-ai-proxy-helper`

There is no standalone `aci` executable. The package neither embeds nor imports
the remote Private AI Gateway server.

## Request Path

```text
coding agent
    -> Local API with an agent-scoped token
    -> private-ai-proxy serve over a verified ACI channel
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
- Rust toolchains required by the manifests
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
cargo build --manifest-path apps/desktop/cli/Cargo.toml --bin private-ai-proxy
```

The Tauri development server is only the renderer transport used by
`tauri dev`. The repository does not ship a browser preview application, a mock
desktop runtime, or screenshot-specific production branches.

## Verification

Run the focused checks from `apps/desktop`:

```bash
npm run check
npm run test:release
npm run test:probe

cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked --manifest-path src-tauri/Cargo.toml
```

The Rust suites cover protocol verification, local proxy behavior, lifecycle,
configuration transactions, recovery, usage, and native command boundaries.
Renderer correctness is checked by TypeScript and exercised through the real
Tauri application; there is no in-page mock API or Playwright screenshot suite.

## Profiles And Credentials

Profiles contain a name, provider, service endpoint, and authentication method.
Provider credentials are stored only in the OS credential store. The renderer,
profile JSON, agent configuration, command arguments, and diagnostics never
receive the raw saved credential.

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

`brand/<id>/brand.json` and the adjacent source assets are the branding source
of truth. Generate tracked outputs with:

```bash
npm run prepare:brand
```

Do not edit generated renderer, Tauri, installer, or tray assets directly. CI
regenerates them and fails on drift. The default application identifier is
`org.dstack.private-ai-proxy` and uses a separate data and credential namespace
from earlier beta builds.

## Packaging And Releases

The desktop workflow builds separate macOS arm64/x64 DMGs, Windows NSIS
installers, and Linux DEB/RPM packages, plus portable CLI archives. AppImage is
not supported because its transient mount is incompatible with a persistent
per-user backend.

Release and updater behavior is documented in
[CLI distribution](docs/cli-distribution.md) and implemented by
`.github/workflows/desktop-native.yml`. Published updates use Tauri's signed
updater artifacts. macOS distribution additionally requires Developer ID
signing and notarization. Stable Windows releases additionally require an
Authenticode certificate; CI imports it only for the package job, configures
Tauri with its thumbprint, signs the bundled executables before packaging,
verifies the installer and installed executables, and removes it afterward.

Beta and stable are independent update channels. A release version, channel,
published assets, and feed metadata must agree; partial platform releases update
only their selected beta feeds. Stable releases must run from `main` and include
all supported platforms. The tag, title, notes, asset naming, signing credentials,
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
