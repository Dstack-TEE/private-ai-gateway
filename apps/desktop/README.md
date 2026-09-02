# Private AI Gateway Desktop

Tauri v2 menu bar app that turns the bundled `aci serve` verifier into a local
agent gateway for Claude Code (Codex and OpenCode are reported as unsupported; see below).

> Every request goes to a hardware-verified private AI service, and every
> response is checked against its signed receipt.

## Architecture

- **Primary instance, then endpoint, then identity.** At launch the app takes
  a per-user OS file lock (`fd-lock`) to become the primary instance, binds
  `127.0.0.1:4180` synchronously and exclusively, and loads the TLS identity;
  any failure is shown in the window and blocks starting the gateway and
  connecting agents for that launch. Disconnecting agents never depends on
  it. A second instance hands off to the first (the Tauri single-instance
  plugin only focuses the window; the lock decides).
- **Local endpoint `https://127.0.0.1:4180`** is served over TLS with a
  per-installation identity: a local CA generated once (key and certificate
  in the OS credential store; the certificate also written owner-only to the
  app data directory), and a server certificate for `127.0.0.1`/`localhost`
  issued from it on every launch whose key never leaves memory. An agent that
  trusts the CA refuses anything else on the port, running or not:
  `tests/tls_identity.rs` proves a rustls client and the installed Claude Code
  CLI both fail before sending credentials against a plain-HTTP or
  foreign-certificate listener.
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
  (Claude Code: Messages and `count_tokens`; Codex: Responses; OpenCode: Chat
  Completions; `/v1/models` for all) plus an attribution label. It does not
  defend against other software running as the same OS user, which can read
  the same files or run the helper; server identity is what the TLS
  certificate provides. Claude Code obtains its token through the bundled
  console helper (`private-ai-gateway-helper --agent-token claude-code`).
- **RedPill API key**, the local CA key, and any credential a connection
  takes over live only in the OS credential store (`keyring` 4). Previews
  show `Existing secret` / `Managed local credential` in place of values; the
  connection record stores an opaque `secret_ref`. Record, tokens, CA file,
  and temp files are owner-only (0600/0700; on Windows they inherit the
  per-user profile ACL) and tightened when read; config writes hold a
  cross-process file lock from the revision check to the final rename.
- **Model catalog** is the verified service's `GET /v1/models`, read through
  the sidecar and published atomically with the identity. It proves
  availability only. A surface is used solely when the service publishes the
  versioned `aci_capabilities` declaration (`{"version":1,"surfaces":{...:"all"}}`)
  for it; this repository's service emits it (chat completions and Messages
  `all` with the control-plane middleware, Responses `undeclared`; everything
  `undeclared` in direct-upstream mode). Against a deployed service that
  predates the declaration every surface is undeclared, requests are refused,
  and no agent can be connected.
- **What a receipt proves.** The verifier applies its ACI policy to inference
  bodies (`provider.aci_verified`, pinned sessions) and re-serializes them;
  the receipt binds those bytes, shown as `ACI policy applied locally`, not
  the agent's original request. A service-side rewrite recorded in the
  receipt shows as `Rewritten by service`.
- **Proxy limits.** Request bodies are buffered (32 MiB, the same limit the
  sidecar enforces, 60 s read timeout) so the model can be checked against
  the catalog; responses stream. At most 64 requests are in flight (`429`);
  upstream connect 5 s, idle read 300 s (`504`). Standard hop-by-hop headers
  plus any named by `Connection`, `Proxy-Connection`, the agent credential,
  and the attribution tag are removed in both directions by both proxies.
  `count_tokens` (Messages) and `responses/compact` (Responses) are
  capability-gated helpers: the same token scope, verified session, declared
  surface, and catalog model check as inference (both protocols require
  `model`, so a body without one is refused). No agent fallback behaviour is
  claimed. TLS handshakes are bounded (64 pending) and each must complete
  within 10 s, so half-open connections cannot pin the listener.

## Agents

| Agent | Status in this build | Config written | Credential reference |
| --- | --- | --- | --- |
| Claude Code | **Enabled** when the bundled helper is present and the service declares the Messages surface | `~/.claude/settings.json`: `env.ANTHROPIC_BASE_URL`, `env.NODE_EXTRA_CA_CERTS` (the installation CA; an official Claude Code setting, honoured from the `env` block), `env.ANTHROPIC_MODEL` (a model you pick from the verified list), `env.CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY`, `apiKeyHelper`; `env.ANTHROPIC_AUTH_TOKEN` / `env.ANTHROPIC_API_KEY` are taken over (they outrank `apiKeyHelper`) and restored on disconnect | helper command |
| Codex | **Unsupported.** Codex's strict catalog loader requires per-model instructions Codex ships only for its own models; the verified service publishes no such metadata. | none | none |
| OpenCode | **Unsupported.** OpenCode (Bun) has no configuration-level way to trust the local certificate; only a shell-exported `NODE_EXTRA_CA_CERTS` would, which the app cannot verify, and falling back to plain HTTP is not acceptable. | none | none |

Connections recorded by an earlier version for an unsupported agent are
disabled on load (their tokens are never authorized again) and shown as
`Needs attention` until disconnected; `Disconnect` and `Restore all` work
without support, endpoint, or gateway. `Connect` previews the exact fields
with a revision of the inputs; `Apply` refuses if any moved. Token, parked
secrets, config, and record are applied as one transaction and rolled back
together. `Disconnect` tombstones the record (disabled, cleanup pending),
restores only fields that still hold what the app wrote, then removes token,
parked secrets, and record; any failure keeps the tombstone for an idempotent
retry, and a new connection never reuses a leftover token. A model that
disappears from the catalog shows `Needs attention`; nothing is switched for
you. Claude Code reloads its settings on change. Shell-exported
`ANTHROPIC_AUTH_TOKEN`/`ANTHROPIC_API_KEY` are invisible to the app and would
still outrank the helper; the preview says so.

An agent row separates three facts: *recorded* (a connection record exists),
*managed* (the config still holds exactly what the app wrote), and
*authorized* (the proxy would accept the agent's token now: recorded, enabled,
and managed). Statuses and authority come from one scan: listing or
refreshing the agents republishes exactly the token set those statuses
authorize (cancelling admitted-but-unsent deliveries), and startup takes the
same path, so a config that was edited, corrupted, or removed outside the app
stops opening the proxy in the same operation that reports `Needs attention`.
`Disconnect` and `Restore all` delete the agents' token files before any
record or config is touched, and the removal is synced to the parent
directory (on Windows, a directory-handle flush; NTFS metadata journaling is
the strongest guarantee available there) before anything else runs: revoking
the capability itself is durable, so no later failure — not even a crash or
power loss — can leave an agent authorized, while the record stays visible
for an idempotent retry. A failed sync fails the disconnect closed.
`Disconnect` removes token, record, and consumed parked secrets and leaves
an unreadable config untouched. Install detection (config directory or CLI
on `PATH`) is informational only and never gates Connect; connecting creates
the official config file from scratch. The `apiKeyHelper` command line is
parsed by a POSIX `sh` on every platform (Git's sh on Windows), so the path
is quoted uniformly with `shlex`. Record and token files are read through
`O_NOFOLLOW` descriptors and reads never change permissions; owner-only
permissions are restored only by explicit maintenance under the apply lock.

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
stdout works on Windows). Both crates declare `rust-version = 1.89`, the highest MSRV in their locked
dependency graphs (`aes` 0.9.3: 1.89; `keyring` 4.2: 1.88), and commit their
`Cargo.lock`; CI builds and tests with `--locked`, checks the gateway crate,
helper, and Tauri backend on Rust 1.89, on Ubuntu and Windows as well as macOS, and smoke-tests the
credential store on macOS, Windows, and (under `dbus-run-session` with
gnome-keyring) Linux.

The `Desktop macOS` GitHub Actions workflow builds an unsigned DMG and ZIP on
`macos-latest`, launches the packaged tray app, runs the bundled sidecar
against `https://tee.redpill.ai`, checks the verified local `/v1/models` path,
and uploads a screenshot plus codesign, Gatekeeper, and size inspection output.
