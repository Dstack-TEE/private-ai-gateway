# Private AI Gateway Desktop

Cross-platform Tauri desktop app that turns the bundled `aci serve` verifier
into a local gateway for Codex, Claude Code, OpenCode, Pi, and Hermes. One Rust
runtime owns policy, persistence, credentials, usage, agent projection, and
process lifecycle. A shared React renderer owns the dense product UI, while
Tauri delegates windows, menus, tray integration, file dialogs, confirmation
dialogs, clipboard, autostart, and application lifecycle to each operating
system.

> Every request goes to a hardware-verified private AI service, and every
> response is checked against its signed receipt.

## Architecture

```
Codex / Claude Code / OpenCode / Pi / Hermes
        │  the agent's own API, a machine-local token
        ▼
configured Local API    in-process Rust proxy: agent tokens, catalog check,
        │               limits, revocation gate, activity; relays unchanged
        ▼
127.0.0.1:<dynamic>     bundled `aci serve`: TEE identity, pinned channel,
        │               policy, forwarding, receipt verification
        ▼
https://tee.redpill.ai
```

The desktop app converts nothing. Whatever an agent sends on
`/v1/chat/completions`, `/v1/messages`, or `/v1/responses` (and the
`count_tokens` / `responses/compact` helpers) reaches the verified service with
the same method, path, query, body, and streaming; the service's status,
headers, and bytes come back the same way. Whether the service answers a
protocol is the service's own response, shown as such.

- **Primary instance, then endpoint.** At launch the app takes a per-user OS
  file lock (`fd-lock`) to become the primary instance and synchronously binds
  the saved Local API address and port; a failure is shown in the window and
  blocks starting and connecting for that launch. Disconnecting agents never
  depends on it. A second instance hands off to the first (the Tauri
  single-instance plugin only focuses the window; the lock decides).
- **Local endpoint** defaults to loopback-only `http://127.0.0.1:4180`. The
  app claims the configured address and port before agent connections are
  available.
- **Sessions.** The proxy forwards only while a *verified session* is
  published: the sidecar's verified identity and the catalog read through it,
  together, under one generation (per sidecar start) and epoch (per identity
  report or refresh). Starting, stopping, `blocked`, `fatal`, or a crash is
  one atomic barrier: the epoch moves, the identity must be reported again,
  the catalog is cleared, and a read still in flight can neither publish nor
  clear the error. Each request holds a lease and re-checks it, plus the
  credential epoch (token still owned by the same agent, key unchanged),
  after the body is read; the send is then raced against a delivery token
  that every revocation cancels, so a request admitted before a Delete key,
  Disconnect, or Stop but not yet sent is refused (`503 revoked`) rather than
  delivered. A failure before `send()` is recorded as `Blocked locally`; once
  upstream delivery begins, a timeout or connection failure is recorded as an
  upstream failure with delivery explicitly unconfirmed, never as "did not
  leave this Mac." Deletes revoke the key in memory before touching the
  credential store. The sidecar re-checks verification for every method and
  refuses to forward when a re-verification changed the service identity
  mid-request.
- **Agent tokens** are random per-agent secrets in owner-only files under the
  app data directory. A token is a capability for that agent's endpoints
  (Claude Code: Messages and `count_tokens`; Codex and Pi: Responses and
  `responses/compact`; OpenCode and Hermes: Chat Completions; `/v1/models`
  for all) plus
  an attribution label the proxy sends as `x-aci-tag`, which the sidecar
  copies into its receipt event and strips before forwarding. It does not
  defend against other software running as the same OS user, which can read
  the same files or run the helper. Codex and Claude Code obtain their token
  through the bundled console helper
  (`private-ai-gateway-helper --agent-token <agent>`). OpenCode reads the
  token file through its `{file:...}` reference; Pi and Hermes use their
  supported command-backed provider credential mechanisms.
- **Confidential AI profile credentials** and any credential a connection
  takes over live only in the OS credential store (`keyring` 4). Each profile
  has its own credential entry; the profile JSON stores only its name,
  provider, endpoint, authentication kind, credential-presence metadata, and
  verification time. Launching the app does not read the credential store;
  the selected credential is loaded only when verification or protection uses
  it, then cleared from proxy memory when protection stops. It is swapped for
  the agent token on the way to the sidecar and never reaches the window.
  Previews show `Existing secret` /
  `Managed local credential` in place of values; the connection record stores
  an opaque `secret_ref`. Record, tokens, and temp files are owner-only
  (0600/0700; on Windows they inherit the per-user profile ACL) and tightened
  when read; config writes hold a cross-process file lock from the revision
  check to the final rename.
- **Confidential AI profiles** are verified before they are saved. A profile
  combines a user-visible name, provider, endpoint, and authentication method.
  A fresh install starts without a profile and opens New Profile when settings
  are first needed; every profile can be deleted, including the last one.
  Settings offers local, self-hosted branding for the Phala and RedPill
  presets plus a custom HTTPS endpoint. New providers or endpoints require a
  new key, so a credential is never silently reused. Profile metadata is
  written atomically and the current API-key authentication model is shaped so
  an OAuth account can be added as another auth kind later. A successful
  `Verify and Save` selects the profile but leaves protection off until the
  user explicitly starts it. Legacy single-service settings are recognized at
  launch, while their credential migrates to the profile entry on first use so
  opening the app does not request credential-store access.
- **Model catalog** is the verified service's `GET /v1/models`, read through
  the sidecar and published atomically with the identity. It is the single
  source of model truth: agents choose from it, the proxy serves it on
  `/v1/models`, and a request whose `model` is not listed is refused before
  it leaves the machine. Models that disappear on a refresh are reported,
  never replaced.
- **Usage history** is written to an owner-only SQLite database in the app
  data directory and has no automatic retention cutoff. Overview shows five
  recent rows plus a complete current-session summary aggregated from SQLite,
  rather than from the 50-row in-memory activity preview. Usage keeps history
  across app restarts, supports agent/model/time filters and cursor pagination,
  and deletes records only after explicit confirmation. CSV cells that could
  be interpreted as spreadsheet formulas are escaped. Token and cost fields
  remain absent when the provider did not report them.
- **The Local API client key** uses `sk-pag-` followed by 64 lowercase hex
  characters generated from 32 random bytes. Existing beta `pag_` keys remain
  valid so upgrades do not silently break configured clients; newly created or
  rotated keys use the current format.
- **What a receipt proves.** The verifier applies its ACI policy to inference
  bodies (`provider.aci_verified`, pinned sessions) and re-serializes them;
  the receipt binds those bytes, shown as `Policy applied`, not the agent's
  original request. A service-side rewrite recorded in the receipt shows as
  `Rewritten by service`.
- **Proxy limits.** Request bodies are buffered (32 MiB, the same limit the
  sidecar enforces, 60 s read timeout) only so the `model` can be checked
  against the catalog; nothing else in the body is read, and responses
  stream. At most 64 requests are in flight (`429`); upstream connect 5 s,
  idle read 300 s (`504`). Standard hop-by-hop headers plus any named by
  `Connection`, `Proxy-Connection`, the agent credential, and the attribution
  tag are removed in both directions by both proxies. The helper endpoints
  are gated exactly like inference: token scope, verified session, and a
  catalog model (both protocols require `model`).

## Agents

| Agent | Config written | Credential reference |
| --- | --- | --- |
| Codex | `~/.codex/config.toml`: required verified `model`, `model_provider`, and a `model_providers.private_ai_gateway` Responses provider | helper command |
| Claude Code | `~/.claude/settings.json`: `env.ANTHROPIC_BASE_URL`, gateway model discovery, `apiKeyHelper`; optional `env.ANTHROPIC_MODEL`; higher-priority exported credentials must be unset | helper command |
| OpenCode | `opencode.json`: an app-owned `@ai-sdk/openai-compatible` provider whose model map is generated from the verified catalog; optional default | token file |
| Pi | `~/.pi/agent/models.json`: an app-owned Responses provider whose models, limits, modalities, reasoning flag, and prices come from the verified catalog | helper command |
| Hermes | `~/.hermes/config.yaml`: a comment-preserving custom Chat Completions provider with `discover_models`, optional default, and command-backed auth | helper command |

The verified catalog is the only model source. Codex requires a selected
verified default because it does not discover this custom provider's model
catalog; the other agents may choose after connecting through native discovery
or an app-owned catalog generated from the verified service. `Connect` previews
the exact fields with a revision of the inputs; generated model maps are shown
as a concise catalog summary instead of serialized JSON. `Apply` refuses if any
moved. Token, parked secrets, config, and record are applied as one transaction
and rolled back together.
`Disconnect` and `Restore all` work without endpoint or gateway.
`Disconnect` tombstones the record (disabled, cleanup pending), deletes the
token file before any record or config is touched, and syncs the removal to
the parent directory (on Windows, a directory-handle flush) before anything
else runs: revoking the capability itself is durable, so no later failure can
leave an agent authorized, while the record stays visible for an idempotent
retry. A failed sync fails the disconnect closed. `Disconnect` removes token,
record, and consumed parked secrets and leaves an unreadable config untouched.
Install detection requires a real executable on `PATH` or in common per-user
and macOS package-manager binary directories; a config directory alone does
not count as an installation. Detection is informational only and never gates
Connect; connecting creates the official config file from scratch. The
`apiKeyHelper` command line is parsed by a POSIX `sh` on every
platform (Git's sh on Windows), so the path is quoted uniformly with `shlex`.
Record and token files are read through `O_NOFOLLOW` descriptors and reads
never change permissions; owner-only permissions are restored only by explicit
maintenance under the apply lock.

## Branding

`brand/<id>/brand.json` is the single source of truth for everything that
names or draws the product: product and organization names, tagline, support
and homepage URLs, the default service URL and key label, the bundle
identifier, category, and descriptions, the accent colours, and the official
asset files next to it. `npm run prepare:brand` (run automatically by
`check`, `build`, `dev`, and `dist`; `PRIVATE_AI_GATEWAY_BRAND=<id>` selects a
brand, default `dstack`) projects it into `src/renderer/generated/` (the
`brand.ts` module plus the light and dark wordmark SVGs, imported as Vite
assets so they ship self-hosted under the production CSP),
`gateway/src/brand.rs`, the cross-platform fallback icons and macOS Icon
Composer asset in `src-tauri/icons`, the template tray icon in `assets/tray`,
and an ignored
`src-tauri/tauri.brand.conf.json` overlay (product name, identifier, bundle
metadata, plus the precompiled native icon list on macOS) that `dev`, `dist`,
and CI pass to the Tauri CLI as
`--config`; the tracked `tauri.conf.json` keeps the window list and stays
neutral, and the window title is set at run time from `brand.rs`. The
committed outputs are for the default brand; CI regenerates them and fails on
drift. With Xcode 26, `prepare-macos-icon.mjs` compiles the `.icon` source into
the native `Assets.car`; it and the PNG, ICO, and ICNS fallbacks all use one
dark-green app icon with the original green Dstack mark. The scripts validate
their inputs and fail fast on a missing field, asset, digest, or named app
icon. Normal and protected tray templates are generated from the same local
mark; the protected variant adds a small status badge at the lower right.

The default brand uses the official Dstack logo kit from
[Dstack-TEE/dstack](https://github.com/Dstack-TEE/dstack) at commit
`982621521b435cc10b535cb8646efecb8c3fc255` (`docs/assets/dstack-logo-kit/`),
with the source paths, licence, and SHA-256 digests recorded in
`brand/dstack/brand.json`. `brand/redpill` and `brand/phala` are templates:
add the official assets they reference before selecting them.

## Development

Install dependencies and run the Tauri app:

```bash
cargo build --bin aci
cd apps/desktop
npm ci
npm run dev
```

Tauri launches the target-triple-specific `aci` binary as an external sidecar.
The development command builds debug sidecars; packaged builds compile release
sidecars from this repository. `npm run dist` produces the native bundle for
the current platform. CI builds the same Tauri application as a macOS DMG and
app, a Windows NSIS installer, and Linux DEB and AppImage packages.

Tests sit at the boundaries. `cargo test --manifest-path gateway/Cargo.toml`
covers the proxy (token scope, fail-closed session, revocation gate, and a
relay check proving that each inference path carries method, path, query,
body, status, and streamed bytes through unchanged), the projections
(round-trip per agent, stale revision, restore all), and the catalog.
`npm run test:renderer` first builds the production renderer, then runs
Playwright against the stateful in-page mock. It covers protection start/stop,
five-agent discovery and reversible config previews, current-session Overview
usage, persistent-history filters and cursor pagination, CSV/clear flows,
proof and local-block semantics, profile management, system confirmation boundaries,
dark/high-contrast/reduced-motion
media, 200% zoom, and
940/720/540/320 widths. CI runs it before all three platform packages.

## Packaging

`npm run dist` builds the release sidecars and runs `tauri build`. Xcode 26 or
newer is required to package the adaptive macOS app icon. The platform bundle
contains the shared renderer, Rust runtime, `aci`, and the credential helper;
there is no second GUI or management service process.

`scripts/bundle-sidecars.mjs` builds two sidecars with `--locked`: the `aci`
verifier and `private-ai-gateway-helper`, a console binary from the gateway
crate that prints an agent's local token (kept separate from the GUI app so
stdout works on Windows). The desktop gateway and Tauri crates declare
`rust-version = 1.89`, the highest MSRV in their locked dependency graphs
(`aes` 0.9.3: 1.89; `keyring` 4.2: 1.88), and commit their `Cargo.lock` files.
The root `aci` follows the root workspace toolchain. CI tests the gateway,
runtime, renderer, and Tauri backend, then compiles and bundles the same app on
macOS, Windows, and Linux. macOS additionally verifies the compiled asset
catalog, legacy ICNS fallback, bundle icon name, DMG, and zipped app bundle.
