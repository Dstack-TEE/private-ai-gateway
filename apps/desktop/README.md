# Private AI Gateway Desktop

Tauri v2 menu bar app that turns the bundled `aci serve` verifier into a local
gateway for Codex, Claude Code, and OpenCode.

> Every request goes to a hardware-verified private AI service, and every
> response is checked against its signed receipt.

## Architecture

```
Codex / Claude Code / OpenCode
        │  the agent's own API, a machine-local token
        ▼
http://127.0.0.1:4180   in-process Rust proxy: agent tokens, catalog check,
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
  file lock (`fd-lock`) to become the primary instance and binds
  `127.0.0.1:4180` synchronously and exclusively; a failure is shown in the
  window and blocks starting and connecting for that launch. Disconnecting
  agents never depends on it. A second instance hands off to the first (the
  Tauri single-instance plugin only focuses the window; the lock decides).
- **Local endpoint `http://127.0.0.1:4180`** is loopback-only HTTP. The app
  claims the port before agent connections are available.
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
  delivered. Deletes revoke the key in memory before touching the credential
  store. The sidecar re-checks verification for every method and refuses to
  forward when a re-verification changed the service identity mid-request.
- **Agent tokens** are random per-agent secrets in owner-only files under the
  app data directory. A token is a capability for that agent's endpoints
  (Claude Code: Messages and `count_tokens`; Codex: Responses and
  `responses/compact`; OpenCode: Chat Completions; `/v1/models` for all) plus
  an attribution label the proxy sends as `x-aci-tag`, which the sidecar
  copies into its receipt event and strips before forwarding. It does not
  defend against other software running as the same OS user, which can read
  the same files or run the helper. Codex and Claude Code obtain their token
  through the bundled console helper
  (`private-ai-gateway-helper --agent-token <agent>`); OpenCode reads the
  token file through its `{file:...}` reference.
- **RedPill API key** and any credential a connection takes over live only in
  the OS credential store (`keyring` 4). The key is loaded into the proxy's
  memory and swapped for the agent token on the way to the sidecar; it never
  reaches the window. Previews show `Existing secret` /
  `Managed local credential` in place of values; the connection record stores
  an opaque `secret_ref`. Record, tokens, and temp files are owner-only
  (0600/0700; on Windows they inherit the per-user profile ACL) and tightened
  when read; config writes hold a cross-process file lock from the revision
  check to the final rename.
- **Model catalog** is the verified service's `GET /v1/models`, read through
  the sidecar and published atomically with the identity. It is the single
  source of model truth: agents choose from it, the proxy serves it on
  `/v1/models`, and a request whose `model` is not listed is refused before
  it leaves the machine. Models that disappear on a refresh are reported,
  never replaced.
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
| Codex | `~/.codex/config.toml`: selected `model`, `model_provider`, and a `model_providers.private_ai_gateway` entry (`base_url`, `wire_api = "responses"`, command-backed `auth`) | helper command |
| Claude Code | `~/.claude/settings.json`: `env.ANTHROPIC_BASE_URL`, selected `env.ANTHROPIC_MODEL`, `apiKeyHelper`; `env.ANTHROPIC_AUTH_TOKEN` / `env.ANTHROPIC_API_KEY` are taken over (they outrank `apiKeyHelper`) and restored on disconnect | helper command |
| OpenCode | `opencode.json`: an `@ai-sdk/openai-compatible` provider with the local `baseURL`, the selected model's name and limits, and a `{file:...}` token reference | token file |

Every agent needs a model from the verified list, so `Connect` is available
once the gateway is verified. `Connect` previews the exact fields with a
revision of the inputs; `Apply` refuses if any moved. Token, parked secrets,
config, and record are applied as one transaction and rolled back together.
`Disconnect` and `Restore all` work without endpoint or gateway.
`Disconnect` tombstones the record (disabled, cleanup pending), deletes the
token file before any record or config is touched, and syncs the removal to
the parent directory (on Windows, a directory-handle flush) before anything
else runs: revoking the capability itself is durable, so no later failure can
leave an agent authorized, while the record stays visible for an idempotent
retry. A failed sync fails the disconnect closed. `Disconnect` removes token,
record, and consumed parked secrets and leaves an unreadable config untouched.
Install detection (config directory or CLI on `PATH`) is informational only
and never gates Connect; connecting creates the official config file from
scratch. The `apiKeyHelper` command line is parsed by a POSIX `sh` on every
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
`gateway/src/brand.rs`, the five desktop icons in `src-tauri/icons`, the
template tray icon in `assets/tray`, and an ignored
`src-tauri/tauri.brand.conf.json` overlay (product name, identifier, bundle
metadata only) that `dev`, `dist`, and CI pass to the Tauri CLI as
`--config`; the tracked `tauri.conf.json` keeps the window list and stays
neutral, and the window title is set at run time from `brand.rs`. The
committed outputs are for the default brand; CI regenerates them and fails on
drift. The script validates everything before writing anything and fails fast
on a missing field, a missing asset, or a changed asset digest.

The default brand uses the official Dstack logo kit from
[Dstack-TEE/dstack](https://github.com/Dstack-TEE/dstack) at commit
`982621521b435cc10b535cb8646efecb8c3fc255` (`docs/assets/dstack-logo-kit/`),
with the source paths, licence, and SHA-256 digests recorded in
`brand/dstack/brand.json`. `brand/redpill` and `brand/phala` are templates:
add the official assets they reference before selecting them.

## Development

Build the Rust CLI once, then run the desktop app:

```bash
cargo build --bin aci
cd apps/desktop
npm ci
npm run dev
```

Tauri launches the target-triple-specific `aci` binary as an external sidecar.
The development command builds a debug sidecar; packaged builds always compile
and bundle a release sidecar from this repository.

Tests sit at the boundaries. `cargo test --manifest-path gateway/Cargo.toml`
covers the proxy (token scope, fail-closed session, revocation gate, and a
relay check proving that each inference path carries method, path, query,
body, status, and streamed bytes through unchanged), the projections
(round-trip per agent, stale revision, restore all), and the catalog.
`npm run test:renderer` runs a Playwright smoke against the built renderer and
the stateful in-page mock (`?mock=interactive`): start/stop with cancel and a
failed-verification retry, key save and delete, connect, Restore all, the
native `<dialog>` confirmation (focus containment, inert background, Escape
returning focus to the trigger), and a 200 % zoom overflow check. CI runs it
on the macOS package job.

## Packaging

`npm run dist` builds the release `aci` sidecar and runs `tauri build`. A macOS
runner produces `Private AI Gateway.app`, a DMG, and a ZIP artifact.

`scripts/bundle-native.mjs` builds two sidecars with `--locked`: the `aci`
verifier and `private-ai-gateway-helper`, a console binary from the gateway
crate that prints an agent's local token (kept separate from the GUI app so
stdout works on Windows). The desktop gateway and Tauri crates declare
`rust-version = 1.89`, the highest MSRV in their locked dependency graphs
(`aes` 0.9.3: 1.89; `keyring` 4.2: 1.88), and commit their `Cargo.lock` files.
The root `aci` follows the root workspace toolchain and currently targets Unix
because the dstack SDK transport uses a Unix-domain socket. CI tests the
gateway crate and helper on Rust 1.89 on Ubuntu and Windows, checks the Tauri
backend on Ubuntu, builds the complete app on macOS, and smoke-tests the
credential store on macOS, Windows, and (under `dbus-run-session` with
gnome-keyring) Linux.

The `Desktop macOS` GitHub Actions workflow builds an unsigned DMG and ZIP on
`macos-latest`, launches the packaged tray app, runs the bundled sidecar
against `https://tee.redpill.ai`, checks the verified local `/v1/models` path,
and uploads a screenshot plus codesign, Gatekeeper, and size inspection output.
