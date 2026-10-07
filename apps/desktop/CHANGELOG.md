# Changelog

## [0.4.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.4.0-beta.1...desktop-v0.4.0) (2026-10-07)


### Miscellaneous Chores

* **desktop:** release 0.4.0 ([#397](https://github.com/Dstack-TEE/private-ai-gateway/issues/397)) ([69898ed](https://github.com/Dstack-TEE/private-ai-gateway/commit/69898ede0e28cfd41af85d858422ae14d7603082))

## [0.4.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.3.0...desktop-v0.4.0-beta.1) (2026-10-07)


### ⚠ BREAKING CHANGES

* **desktop:** `pap profiles login --callback-stdin` is removed; approve the device code that `pap profiles login` prints instead.

### Features

* **desktop:** sign in to RedPill with a device code ([#395](https://github.com/Dstack-TEE/private-ai-gateway/issues/395)) ([5a0311e](https://github.com/Dstack-TEE/private-ai-gateway/commit/5a0311e684b9d2da52265f19abe46921bab43ed5))


### Bug Fixes

* **desktop:** release notes list the downloads again ([#380](https://github.com/Dstack-TEE/private-ai-gateway/issues/380)) ([06a6b20](https://github.com/Dstack-TEE/private-ai-gateway/commit/06a6b208416392e1bde3075a54fd4071f8c13246))

## [0.3.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.1...desktop-v0.3.0) (2026-09-29)

Private AI Proxy 0.3.0 adds DeepSeek Harness and makes the whole app leaner, with the same look and behaviour.

### Features

* **DeepSeek Harness (`dsh`).** Connect it with one switch like the other agents. New dsh sessions (web, desktop, `dsh headless` and editor sessions through `dsh acp`) use the verified Local API. DeepSeek's own web search is turned off while connected so search queries don't bypass the proxy. Disconnecting restores your dsh configuration byte for byte. The `sdk` and `sdk-minimal` profiles pick their provider in code and are not covered.

### Bug Fixes

* **Your config formatting is kept.** JSON settings for Claude Code, OpenCode and Pi are now edited in place, so indentation and spacing you chose survive connect and disconnect. A settings file with duplicate keys is refused instead of silently losing one.
* **Tray.** Agents no longer show a check mark before their status has loaded.
* **CLI.** `pap verify --nonce` values are URL-encoded, so a nonce with `&` or `#` no longer changes the request.
* **Local API example.** The copy-paste `curl` example shows the endpoint unquoted and the key in single quotes.

### Internal

* About 1,100 lines of product code removed across the backend, CLI, native shell, renderer, agent adapters and release tooling. Every config-like format is read and written through a library.
* The nightly live end-to-end test now covers all eight agents, OpenCode 2 and a real network outage.

### Upgrade notes

* A Local API address written in a non-canonical form (port 80, or an IPv6 address that isn't compressed) is rewritten once in canonical form after upgrading. Connections keep working.

## [0.3.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.1...desktop-v0.3.0-beta.1) (2026-09-29)


### Features

* **desktop:** support DeepSeek Harness ([#368](https://github.com/Dstack-TEE/private-ai-gateway/issues/368)) ([df032ed](https://github.com/Dstack-TEE/private-ai-gateway/commit/df032ed046dfdbd558d54500d3c5466de946d667))

## [0.2.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0...desktop-v0.2.1) (2026-09-28)

Private AI Proxy 0.2.1 hardens OpenCode 2 support, restores agents when the app is uninstalled, and cleans up the app's internals with no visible change.

### Bug Fixes

* **OpenCode 2.** OpenCode 2 reads the configuration Private AI Proxy writes, so connecting works as before. A native OpenCode 2 `providers.private-ai-proxy` entry could override the proxy's endpoint while the app still showed Connected; it is now reported as a conflict and the connection stays closed.
* **Uninstall restores agents.** `pap stop --offline` ends the protection session and restores every agent's own configuration without a running backend. The Windows uninstaller runs it (never during in-app updates); if the restore fails it explains why and lets you keep the restore data. On macOS and Linux, choose Stop All and Quit, or run `pap stop --offline`, before removing the app.
* **Reduce Motion.** With Reduce Motion on, every animation on the protection card stops.

### Internal

* The UI API request and response types are generated from the Rust definitions, so a mismatch fails the build.
* Renderer rows and notices are composed from the standard components; every screen renders the same as 0.2.0.
* A nightly live end-to-end test covers all seven agents, OpenCode 2, and a real network outage.

## [0.2.0](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.1.6...desktop-v0.2.0) (2026-09-28)

Private AI Proxy 0.2.0 is the first stable release since 0.1.6. It rebuilds the app around one local API, plain settings files and a lean, native-feeling window.

### Highlights

* **Settings you can read and edit.** `~/.config/private-ai-proxy/config.toml` holds settings and `credentials.toml` (mode 0600) holds API keys and the web UI password. The OS keychain is no longer used. On macOS App Store builds the files stay in the app container. `pap settings` edits them and keeps your comments.
* **Fail closed, everywhere.** Agents stay pointed at the proxy when verification fails, the network drops, or the app restarts for an update; the proxy refuses requests until the service verifies again. Only an explicit stop, disconnect, Stop All and Quit, or Reset settings restores an agent's original configuration.
* **Agents.** Codex 0.157.1 (TEE models only in `/model`; the app offers to stop Codex's background service so new settings apply), Claude Code 2.1.242+, OpenCode, Pi, Oh My Pi, OpenClaw and Hermes, with install-independent detection. Proxied models show as `name [TEE]`.
* **Receipts.** Every delivered response is audited after delivery; request details show the raw signed receipt, and a failed audit names the check that failed.
* **Web UI.** One setting, a generated rotatable password, and cookie sessions.
* **Clear answers.** A refusal says why: requests during an outage get `503 gateway_not_verified` and resume on their own, a missing key names the Local API key, and busy, unreachable or unavailable services are named.
* **Native feel.** Errors appear in a dialog, background events as system notifications; no web-style toasts. Dialogs keep a stable height and scroll inside; the default window fits every page.
* **CLI.** `pap curl` sends one request pinned to the verified TLS key; `--json` failures report the backend's error code in `error.code`.
* **Distribution.** Per-channel update feeds, npm platform binaries as versions of one package, Linux packages without maintainer scripts, SBOMs and build attestations for every release.

### Upgrade notes

* 0.1 settings and keychain credentials are imported once on first launch, then the keychain entries are removed. Don't sync `credentials.toml` to a public dotfiles repository.
* If the keychain can't be read during that import, the import is not retried; profiles that lost their key ask you to sign in again.
* The management API is HTTP on a new local endpoint; 0.1 `pap` clients cannot manage a 0.2 backend. `pap status --json` reports `backend.apiVersion`.
* Receipt audits report after delivery and never block a response.
* "Production OS image" is the service-reported RTMR3 `os-image-hash`; it is not bound to MRTD/RTMR0–2.
* Before uninstalling, use Stop All and Quit (or `pap service stop`) so agents are restored.
* Windows builds are not Authenticode-signed yet; in-app updates are signature-checked.

## [0.2.0-beta.11](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.10...desktop-v0.2.0-beta.11) (2026-09-27)


### Bug Fixes

* **desktop:** fail closed on verification failure and harden the 0.1 upgrade path ([#347](https://github.com/Dstack-TEE/private-ai-gateway/issues/347)) ([f31fc7f](https://github.com/Dstack-TEE/private-ai-gateway/commit/f31fc7f009348b0d8e58da0cfe9e7a8ef1a6cf33))
* **desktop:** show failure reasons and report errors in dialogs ([#346](https://github.com/Dstack-TEE/private-ai-gateway/issues/346)) ([9402c4c](https://github.com/Dstack-TEE/private-ai-gateway/commit/9402c4cbe7b16636231cf487b9f7068bbff6af0b))
* **desktop:** wait for the api.sock endpoint in the App Store smoke test ([#348](https://github.com/Dstack-TEE/private-ai-gateway/issues/348)) ([89f7e2d](https://github.com/Dstack-TEE/private-ai-gateway/commit/89f7e2dc51a57d21f6f91995eb86b8e1d7129a3e))

## [0.2.0-beta.10](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.9...desktop-v0.2.0-beta.10) (2026-09-27)


### Features

* **desktop:** offer to restart Codex's background service after connecting ([#342](https://github.com/Dstack-TEE/private-ai-gateway/issues/342)) ([74685e8](https://github.com/Dstack-TEE/private-ai-gateway/commit/74685e82b5f38df024d2271838764577d140b227))


### Bug Fixes

* **desktop:** native feedback instead of web toasts ([#340](https://github.com/Dstack-TEE/private-ai-gateway/issues/340)) ([eb200d4](https://github.com/Dstack-TEE/private-ai-gateway/commit/eb200d434ad132aea87ecf330f32fafef943aed9))
* **desktop:** support the Codex 0.157.1 baseline and explain its background server ([#339](https://github.com/Dstack-TEE/private-ai-gateway/issues/339)) ([d9d3f58](https://github.com/Dstack-TEE/private-ai-gateway/commit/d9d3f58dd849efb63feceabce6c0c33df16dafd6))

## [0.2.0-beta.9](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.8...desktop-v0.2.0-beta.9) (2026-09-27)


### Features

* **desktop:** keep settings in ~/.config/private-ai-proxy on every platform ([#334](https://github.com/Dstack-TEE/private-ai-gateway/issues/334)) ([f0074a7](https://github.com/Dstack-TEE/private-ai-gateway/commit/f0074a7ebf7674b21f2304d3c41ac4da1f149479))


### Bug Fixes

* **desktop:** center the update button; align the update flow with the updater docs ([#335](https://github.com/Dstack-TEE/private-ai-gateway/issues/335)) ([86d5b46](https://github.com/Dstack-TEE/private-ai-gateway/commit/86d5b465f0ce7b4265f103e837146891fa240917))

## [0.2.0-beta.8](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.7...desktop-v0.2.0-beta.8) (2026-09-27)


### Bug Fixes

* **desktop:** restore grouped lists and fit the default window ([#330](https://github.com/Dstack-TEE/private-ai-gateway/issues/330)) ([f7ea7e0](https://github.com/Dstack-TEE/private-ai-gateway/commit/f7ea7e09110683ae27f1b02116ea07b7e1b1937e))

## [0.2.0-beta.7](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.6...desktop-v0.2.0-beta.7) (2026-09-26)


### Bug Fixes

* **desktop:** final audit fixes for beta.6 ([#325](https://github.com/Dstack-TEE/private-ai-gateway/issues/325)) ([6061f60](https://github.com/Dstack-TEE/private-ai-gateway/commit/6061f601f7294bd6686e670ad10222e0b87ef8f0))
* **desktop:** restore the shadcn default styles ([#324](https://github.com/Dstack-TEE/private-ai-gateway/issues/324)) ([720292f](https://github.com/Dstack-TEE/private-ai-gateway/commit/720292f21f1bd9a38bb0d52cb4fbb8faa618a476))
* **desktop:** show a signed receipt that is not valid JSON as returned ([1db5247](https://github.com/Dstack-TEE/private-ai-gateway/commit/1db52476442485c37cfe74c11d811ea08be6c243))
* **desktop:** starting or stopping protection can no longer run twice from a double click ([6457a68](https://github.com/Dstack-TEE/private-ai-gateway/commit/6457a68466f645266c92ffede730b7dea88078c4))

## [0.2.0-beta.6](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.5...desktop-v0.2.0-beta.6) (2026-09-26)


### Features

* **desktop:** add pap curl for one request pinned to the verified TLS key ([#319](https://github.com/Dstack-TEE/private-ai-gateway/issues/319)) ([a346e7a](https://github.com/Dstack-TEE/private-ai-gateway/commit/a346e7adbef4e86532b2b8f0fcd636c8ffa8f5d8))
* **desktop:** native look and behaviour on macOS, Windows and Linux ([#317](https://github.com/Dstack-TEE/private-ai-gateway/issues/317)) ([7a90e89](https://github.com/Dstack-TEE/private-ai-gateway/commit/7a90e89cc04ccb143969a125e02718e290f6a77b))


### Bug Fixes

* **desktop:** a late command result no longer overwrites newer state ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** cancelling the web UI import picker no longer leaves the dialog stuck ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** screen readers read usage token counts ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** Settings from the macOS menu or tray opens once ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** tray and menu navigation waits for an open dialog instead of being dropped ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** update checks continue while the window is hidden ([32f1bd6](https://github.com/Dstack-TEE/private-ai-gateway/commit/32f1bd6dae6ad95e913f5197c407a3d08a4e2965))
* **desktop:** web UI opens with a generated, rotatable password ([#316](https://github.com/Dstack-TEE/private-ai-gateway/issues/316)) ([548f265](https://github.com/Dstack-TEE/private-ai-gateway/commit/548f265c8be5446188076de939d097e6f4e977d0))

## [0.2.0-beta.5](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.4...desktop-v0.2.0-beta.5) (2026-09-26)


### Features

* **desktop:** show proxied model names with a [TEE] suffix ([#312](https://github.com/Dstack-TEE/private-ai-gateway/issues/312)) ([f98a0b6](https://github.com/Dstack-TEE/private-ai-gateway/commit/f98a0b6763d36032a3300a4b7fe8be40a97660d9))

## [0.2.0-beta.4](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.3...desktop-v0.2.0-beta.4) (2026-09-26)


### Features

* **desktop:** raw receipts in request details; single-entry password forms ([#308](https://github.com/Dstack-TEE/private-ai-gateway/issues/308)) ([81ef1f4](https://github.com/Dstack-TEE/private-ai-gateway/commit/81ef1f4be755355bfd7f4358d2b9b1f0ba5f7bae))


### Bug Fixes

* **desktop:** dialog exit animations and content lifecycle ([#309](https://github.com/Dstack-TEE/private-ai-gateway/issues/309)) ([b23f6fb](https://github.com/Dstack-TEE/private-ai-gateway/commit/b23f6fb693a03c0deff83dd7b0308df52e58ac50))
* **desktop:** install-independent agent detection and Oh My Pi connect ([#310](https://github.com/Dstack-TEE/private-ai-gateway/issues/310)) ([24fda44](https://github.com/Dstack-TEE/private-ai-gateway/commit/24fda440999475e992c985fb58bc36357ab3a2a4))
* **desktop:** no focus ring on programmatic focus ([#306](https://github.com/Dstack-TEE/private-ai-gateway/issues/306)) ([364037f](https://github.com/Dstack-TEE/private-ai-gateway/commit/364037fad4ee98a3f1c9cc8cd84708445f3926cc))

## [0.2.0-beta.3](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.2...desktop-v0.2.0-beta.3) (2026-09-25)


### Features

* **desktop:** check key custody under a configured policy ([3bf5e44](https://github.com/Dstack-TEE/private-ai-gateway/commit/3bf5e44ec73747843c63d1d7cb198f5a09ef5ad7))


### Bug Fixes

* **desktop:** final backend audit fixes ([#287](https://github.com/Dstack-TEE/private-ai-gateway/issues/287)) ([4cdd927](https://github.com/Dstack-TEE/private-ai-gateway/commit/4cdd92743a359605fbc71c9a0ff5d4d683c30c18))
* **desktop:** keep the tray agents menu and protection text current ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** new profiles default to RedPill, the default provider ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** open the window in the saved appearance before its first paint ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))
* **desktop:** pin only the TLS entry the report declares ([3bf5e44](https://github.com/Dstack-TEE/private-ai-gateway/commit/3bf5e44ec73747843c63d1d7cb198f5a09ef5ad7))
* **desktop:** save "Allow development OS" so the window, tray and web UI agree ([ee16e3e](https://github.com/Dstack-TEE/private-ai-gateway/commit/ee16e3ee4e5d51f6ea70121a945e0c360765ae1f))

## [0.2.0-beta.2](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.2.0-beta.1...desktop-v0.2.0-beta.2) (2026-09-25)


### Bug Fixes

* **desktop:** heal a stale TLS pin through the rotation path instead of wedging in 502 ([#285](https://github.com/Dstack-TEE/private-ai-gateway/issues/285)) ([b8159fb](https://github.com/Dstack-TEE/private-ai-gateway/commit/b8159fba35c6a4360e85ef4ddc753a569f221eb3)), closes [#241](https://github.com/Dstack-TEE/private-ai-gateway/issues/241)
* **desktop:** publish the feed and npm after a skipped App Store job ([#282](https://github.com/Dstack-TEE/private-ai-gateway/issues/282)) ([dd770e9](https://github.com/Dstack-TEE/private-ai-gateway/commit/dd770e9462670a4f7964f8b0323243fca68a0022))

## [0.2.0-beta.1](https://github.com/Dstack-TEE/private-ai-gateway/compare/desktop-v0.1.7-beta.4...desktop-v0.2.0-beta.1) (2026-09-25)


### ⚠ BREAKING CHANGES

* **desktop:** `--json` failures report the backend's error code in `error.code` with the message alone in `error.message`; previously the code was always `command_failed` and the message was prefixed with `CODE: `.
* **desktop:** The management API is HTTP on a new local endpoint (Unix `api.sock`, Windows pipe `<id>-<hash>-api`); 0.1 clients cannot manage a 0.2 backend, and 0.2 clients stop 0.1.4+ backends on update. `pap status --json` and `pap service start --json` report `backend.apiVersion` instead of `protocolVersion`. Failed commands report the backend's error codes: `pap usage show` of an unknown record says `not_found: Usage record not found`, and the `encoding_failed` and `incompatible_protocol` codes are gone. Web UI RPC paths are the snake_case command names and errors use each code's HTTP status.

### Features

* **desktop:** config.toml and credentials.toml replace the JSON settings and the OS keychain ([#265](https://github.com/Dstack-TEE/private-ai-gateway/issues/265)) ([1dfda3b](https://github.com/Dstack-TEE/private-ai-gateway/commit/1dfda3b0366205eeaa1e53867a2fefae418c4acb))
* **desktop:** release-please versions, changelog and tag-triggered releases ([#272](https://github.com/Dstack-TEE/private-ai-gateway/issues/272)) ([42b5a60](https://github.com/Dstack-TEE/private-ai-gateway/commit/42b5a60369d85687eb683d215b650c5913885533))
* **desktop:** route renderer pages with TanStack Router ([#261](https://github.com/Dstack-TEE/private-ai-gateway/issues/261)) ([333609a](https://github.com/Dstack-TEE/private-ai-gateway/commit/333609a26f69f300488696d1e1962bc5d6b7cfa2))
* **desktop:** single Web UI setting with password sign-in and cookie sessions ([#262](https://github.com/Dstack-TEE/private-ai-gateway/issues/262)) ([a9644d2](https://github.com/Dstack-TEE/private-ai-gateway/commit/a9644d2e019c92669929e3490c59ef192d71d77c))


### Bug Fixes

* **desktop:** backend review follow-ups ([#281](https://github.com/Dstack-TEE/private-ai-gateway/issues/281)) ([03eb86c](https://github.com/Dstack-TEE/private-ai-gateway/commit/03eb86c0b90af835252405a3d6f168d187debae7))
* **desktop:** nfpm-built Linux packages without maintainer scripts; web UI and helper precedents ([#269](https://github.com/Dstack-TEE/private-ai-gateway/issues/269)) ([6d9661a](https://github.com/Dstack-TEE/private-ai-gateway/commit/6d9661a116a39a1f35e6adfe319df589cee96739))
* **desktop:** pre-release backend audit fixes ([#280](https://github.com/Dstack-TEE/private-ai-gateway/issues/280)) ([5a09074](https://github.com/Dstack-TEE/private-ai-gateway/commit/5a09074ef65194263a5539838c66ee3d69a892e3))
* **desktop:** pre-release UI and CLI audit fixes ([#279](https://github.com/Dstack-TEE/private-ai-gateway/issues/279)) ([ab8e5fc](https://github.com/Dstack-TEE/private-ai-gateway/commit/ab8e5fc6c3a3d70039cf1cda0f9cd58ef2d0fb6f))
* **desktop:** publish npm platform binaries as versions of one package, like Codex ([#264](https://github.com/Dstack-TEE/private-ai-gateway/issues/264)) ([83989e0](https://github.com/Dstack-TEE/private-ai-gateway/commit/83989e0f427c030c0136d22cc3adc65993d09150))
* **desktop:** read one static latest.json per update channel ([#271](https://github.com/Dstack-TEE/private-ai-gateway/issues/271)) ([84dfcc6](https://github.com/Dstack-TEE/private-ai-gateway/commit/84dfcc6ffb52a47a372c608344d318dd45bbf572))
* **desktop:** settings file contract and a device-local, retryable 0.1 import ([#266](https://github.com/Dstack-TEE/private-ai-gateway/issues/266)) ([e5c4e81](https://github.com/Dstack-TEE/private-ai-gateway/commit/e5c4e81055ba9308c399cf84301348c99d25673a))


### Miscellaneous Chores

* **desktop:** release 0.2.0-beta.1 ([a540870](https://github.com/Dstack-TEE/private-ai-gateway/commit/a540870d2028cea239c5de2d4de6a23d1c2f2204))


### Code Refactoring

* **desktop:** one HTTP API for the local endpoint and the web UI ([#276](https://github.com/Dstack-TEE/private-ai-gateway/issues/276)) ([a56af02](https://github.com/Dstack-TEE/private-ai-gateway/commit/a56af02b2a4b546c12d678b5c8c569feb04fa24d))
