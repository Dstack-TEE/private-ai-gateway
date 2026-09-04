# Private AI Gateway Desktop Product Readiness

This checklist is the release contract for the desktop app. A checked item has
an automated assertion, a focused code-level test, or an explicitly documented
platform verification result.

## Protection And Failure Semantics

- [x] A request can leave the device only while identity and catalog belong to the same verified epoch.
- [x] Missing key, unavailable port, stale identity, sidecar exit, catalog loss, and revoked credentials fail closed with an actionable message.
- [x] Local rejection is distinguishable from an upstream failure and records `left_device = false`.
- [x] Failures after upstream delivery begins record `left_device = true` and state that receipt by the service is unconfirmed.
- [x] Tray, window header, Overview, and dialogs use the same protection state and reason.

## Usage

- [x] Usage persists in an owner-only SQLite database across app restarts.
- [x] Usage has no automatic retention cutoff and is removed only by explicit Clear History.
- [x] The Overview session resets on a new protection session, shows up to five recent records, and aggregates the complete session from SQLite.
- [x] The Usage page filters by agent, model, and time range.
- [x] Usage provides cursor pagination, request/token/cost/protected summaries, and a chart.
- [x] Token and cost values are captured from real response usage only; unavailable values remain unknown.
- [x] CSV export uses a native save dialog, escapes spreadsheet formulas, and history clearing requires confirmation.
- [x] Receipt, local-block, rewrite, and proof details remain inspectable.

## Model Catalog

- [x] The verified `/v1/models` response is the only model source.
- [x] Invalid price, TEE, modality, and capability metadata is omitted rather than invented.
- [x] Catalog is collapsed by default, scrollable, counted, and has a sticky header.
- [x] TEE badges appear only when the service returned `is_tee: true`.

## Agents

- [x] Codex, Claude Code, OpenCode, Pi, and Hermes are detected from real executables.
- [x] Detection covers PATH and common macOS/user package-manager binary locations.
- [x] Connections use native discovery or an app-owned catalog generated from the verified service.
- [x] Codex requires a verified default model; other agents may choose after connecting; no full model list is handwritten in source.
- [x] Generated provider catalogs are summarized in previews instead of rendering serialized JSON.
- [x] Preview/apply uses a revision and refuses stale config or catalog inputs.
- [x] Disconnect and Restore All revoke tokens before config cleanup and preserve unrelated user config.
- [x] Official agent icons have a robust fallback.

## macOS Product Surface

- [ ] Native window, traffic lights, sidebar, page headers, dialogs, and tray menu align to the app window.
- [x] Tray includes Protection, status, Open, Settings, Open at Login, and Quit.
- [x] Autostart failures are visible in the window and tray state remains truthful.
- [x] Open at Login uses a per-user LaunchAgent, requests no Automation access, and starts hidden behind the tray.
- [x] Closing hides the window while Quit stops the gateway and exits.

## Interaction And Accessibility

- [x] Icon controls have labels/tooltips, dialogs restore focus, and navigation moves focus to page headings.
- [x] Busy, disabled, empty, error, and confirmation states are present and actionable.
- [x] No nested interactive controls; keyboard and screen-reader status updates are supported.
- [x] Reduced motion, dark mode, high contrast, 200% zoom, and 940/720/540/320 widths are tested.
- [x] No horizontal overflow, incoherent overlap, clipped control text, or product text below 12px.

## Release Verification

- [x] TypeScript check and production renderer build pass.
- [x] Renderer interaction, accessibility, and geometry tests pass.
- [x] Gateway Rust unit/integration tests pass.
- [x] Tauri Rust formatting and tests pass in an isolated Linux build container.
- [x] `git diff --check` passes and generated caches/processes are cleaned up.
- [ ] macOS-only tray, autostart, traffic lights, signing, and DMG behavior are verified on macOS before release.

Linux verification note (September 3, 2026): the host has no Rust toolchain or
Tauri system packages. An isolated Rust 1.89 container with the required Linux
build libraries passed all Tauri tests without installing global packages.
Native macOS behavior remains a required release check on macOS.
