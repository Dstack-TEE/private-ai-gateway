# Desktop Acceptance Audit

Historical baseline: `9e528c6`, plus the fixes documented below. This is an evidence ledger,
not a claim of complete product, platform or accessibility certification.
Renderer tests exercise the real components against a mock bridge; gateway and
runtime tests exercise filesystem, SQLite, local HTTP and policy behavior.

## Stable Candidate Review (2026-09-08)

- Beta.19 (`c03eec2`) passed 66 renderer tests and all three native CI jobs.
  macOS Developer ID signing, notarization and package inspection passed in
  https://github.com/Dstack-TEE/private-ai-gateway/actions/runs/34182726717.
- At the default 1052x720 content size, Overview's content scroll height equals
  its client height (670px); all four agent rows fit. This is browser layout
  evidence, not native multi-monitor restoration evidence.
- Main `c2d31a8` was subsequently merged into the candidate branch. It includes
  tenant identity changes and the Chutes per-instance evidence memory fix.
- Codex metadata export now has a 15-second deadline with concurrent pipe
  reads and explicit child termination/reaping on failure. This bounds the time
  the external CLI can hold an agent configuration transaction. The regression
  runs inside Tokio and confirms timeout releases the configuration lock.
- Local gateway validation after the fix: 77 passed, one platform keyring test
  ignored; Clippy passed with warnings denied. Beta.19 does not contain this fix.
- After the main merge, all 51 middleware completion and 20 service integration
  tests passed, including stable Chutes sessions across evidence rounds.
- The two tenant identity compatibility tests passed. Shared runtime validation:
  45 passed, two installation-dependent tests ignored.
- Candidate `476e448` passed all three native jobs, renderer and Rust checks in
  https://github.com/Dstack-TEE/private-ai-gateway/actions/runs/34190041787.
  The new macOS package passed Developer ID signing, notarization, backend tests
  and package inspection. This run produced test artifacts without publishing a
  release or advancing an update channel.

Stable acceptance remains open for the new candidate:

| Required evidence | Current status |
| --- | --- |
| Fresh native builds after the main merge and timeout fix | Passed for `476e448`; see the run above |
| Installed beta.18 to beta.19 upgrade, restart and profile preservation | Requires a real installed-app test |
| Sleep/wake, login launch, notification permission denial/regrant, forced exit and recovery | Requires native platform acceptance |
| Windows Authenticode installer/executable signing | Not configured in the current workflow; Tauri update signatures do not satisfy this |

For Windows, first select a distribution signer (for example Microsoft Artifact
Signing or a supported certificate-backed signing service), supply its account
and signing authorization, then verify the installer and every shipped PE
binary. Do not infer OS distribution signing from an updater `.sig` file.
Linux package/repository signing requires a separate distribution decision;
current Tauri update signatures authenticate updates, not apt/rpm repositories.

Naming and CLI integration proposal: [Client architecture](CLIENT-ARCHITECTURE.md).

## Confirmed Corrections

- Receipt enforcement: `aci serve --verify-receipts` now verifies before
  response delivery. Actual HTTP tests cover valid receipts, missing receipts,
  tampered JSON/SSE and receipt unavailability, checking withheld response bytes.
  Earlier desktop betas audited receipts after streaming and cannot retrospectively
  withdraw responses already delivered. Strict mode buffers in memory, not on disk.
- Theme follows System before React mounts, and native dialog backing surfaces
  use the parent window's effective appearance. Profiles select the entire row;
  proof content shares the heading/footer inset and separates verbose reports.

1. Periodic Agent reconciliation previously republished unchanged token sets and
   cancelled admitted requests. Identical sets now leave the credential epoch
   and cancellation gate untouched. Actual revocations still cancel delivery.
2. Agent list scans and client-key rotation now use the same policy lock as
   connect/disconnect, preventing stale token snapshots from restoring access.
3. Client-key rotation retains revoke-first behavior on storage failure. Other
   agent credentials remain unchanged, but the confirmation now correctly warns
   that in-flight requests can be interrupted by a credential mutation.
   Failed rotations broadcast availability (never the secret) and clear cached
   keys in open windows so revoked credentials cannot be copied as current.
4. Configuration-only verification disables the protection switch instead of
   offering cancellation that the backend lifecycle transaction cannot perform.
5. A native editor for a deleted/nonexistent profile displays a dismissible error
   instead of silently becoming a New Profile form.
6. Failed usage queries clear stale results and show unknown totals, not results
   from previous filters. Selected agent/model labels survive empty facets after
   clearing history. Charts do not invent zero usage when a query failed.

## Workflow Coverage

### UI Consistency Follow-up (2026-09-06)

- Warning uses amber independently of destructive errors; primary uses Neutral.
  Overview decoration uses neutral primary; protection switches use success,
  with warning taking precedence in development mode.
- Live Verified is a small, right-aligned button opening Privacy Verification.
- Forms use standard FieldSet/FieldSeparator and a shared footer divider.
  Settings error alerts sit outside row groups to avoid double separators.
  Local API option rows retain borders. Endpoint previews have been removed;
  Overview help opens model-aware cURL/Python/JavaScript examples with the current
  local client key, read on demand. Provider credentials are never embedded.
- Settings, profiles and usage share an Item-based action row with full-row hover.
  Provider logos are 18px; chart metrics use standard Tabs with visible selection.
- Profile forms omit the credential-delete action and verification badge.
  Verify and Save remains intentional: invalid replacement credentials must not
  overwrite a working profile or trigger an unverified reconnection.
- Agent detection is automatic on startup and window activation; connection mutations show optimistic
  state and progress without disabling unrelated agents. Restore all is locked
  during a connection mutation. Model-sync filler text is removed.
- No manual detection control remains. Writes
  serialize per agent and coalesce repeated input to the latest intent. Tauri
  filesystem, SQLite and credential commands execute on blocking workers, not
  the UI thread. Exit restores configurations asynchronously before allowing exit.
- Usage uses shadcn Chart/Recharts stacks per model, with full-filter SQL totals,
  zero-filled dates and monthly aggregation for long ranges. Filters use local
  calendar-day boundaries, including Today. Production-CSP
  coverage checks rendered bars without allowing dynamic style tags.
- Chart colors use a separate categorical palette and the complete model facet.
  Other preserves totals beyond ten model series. Calendar ranges apply to
  summaries, charts, cursor-paginated Table rows and CSV with an exclusive upper
  date boundary. Table/Overview share outcomes, formatting and proof navigation.
- About keeps version and update status/action on one row. Update confirmation
  is native; progress reuses the native child-window infrastructure and standard
  Progress content (Dialog in browser preview). A backend snapshot prevents lost
  progress/errors during window startup. Active installation cannot be closed.

Renderer checks do not certify native window behavior. Linux desktop build
dependencies are installed and native validation uses the project's Rust 1.89
toolchain. macOS sheet presentation, Windows/Linux close handling, and signed
upgrade/restart still require platform acceptance.

| Workflow | Automated evidence | Remaining acceptance |
| --- | --- | --- |
| First launch | No-profile opens New Profile; first provider preset, validation and failed-save behavior | Real credential permission prompts |
| Profiles | Presets, scoped keys, verify/save without starting protection, switching, deletion, child dialogs, stale editors | Native sheets and OS credential failures |
| Protection | Verified identity, fail-closed states, startup cancellation, configuration lockout, session clock | Real provider verification and disconnects |
| Agent configuration | Five integrations, verified model discovery, drift detection, uninstall, restore/retry, persistent links | Actual CLI sessions with installed versions |
| Admission/revocation | Local HTTP tests: credentials revoked during body/read/send boundaries; unchanged token reconciliation does not cancel delivery | Long sessions on each target OS |
| Local API | Endpoint validation, occupied-port rollback, rebind serialization, network opt-in, client-key rotation and failure revocation | Real macOS/Windows network permissions |
| Usage | SQLite reopen/persistence, filters, cursor pagination, legacy IDs, full session totals, empty days, CSV safety, explicit clearing | Sustained real traffic and disk exhaustion |
| Proof | Event merging preserves receipt results and usage; forwarded failures distinguished from local rejections | Real signed receipts from each supported provider |
| Updates | Standard Tauri updater and SemVer libraries; channel isolation, persistence, automatic checks, offline retry, missing-feed state, confirmation | Installed-version upgrade/restart and rollback acceptance |
| Settings | Grouped shadcn Item/Field composition, Advanced channel selector, independent local version display | WKWebView font/rendering acceptance |
| Navigation/dialogs | Keyboard navigation, nested dialogs, focus return, no transient loading frame, Profile opens dialog | VoiceOver and native window focus |
| Layout | Four Overview rows, bounded widths down to 320px, 200% zoom, dark/high-contrast/reduced-motion checks | Human visual approval on macOS |
| Tray/startup/quit | Native code uses shared runtime operations; runtime enforces projection only while connected and protected | Native tray interactions, login launch and quit recovery |

## Historical Verification

- Renderer: 39 tests cover native close-request dispatch and save guards, nested
  focus return, fixed-sidebar shortcuts, native editing-menu dispatch, browser-navigation suppression,
  examples, pre-React system-dark styling, appearance persistence, Settings
  shortcuts and activation-triggered uninstall detection in addition to UI flows.
- Gateway: 45 tests passed; one OS keyring integration test intentionally ignored.
- Runtime: 19 tests passed, including exit retry and post-exit mutation rejection.
- Release tooling: 4 tests passed.
- ACI CLI: 49 passed, one live pin-mismatch test ignored. Strict receipt mode
  rejects missing/tampered/unavailable proof before sending response bytes.
- No provider secrets or production traffic were used for these checks.
- The paused-delivery regression uses notifications, not timing guesses, to
  exercise a reconciliation between admission and upstream send.

## Release Boundaries

- Controls retain shadcn Luma's Neutral primary palette; categorical chart
  colors and semantic success/warning colors are independent. Product-specific
  layouts compose those primitives.
- Profile selection is a dialog workflow, not an inline menu/Select replacement.
- Beta and stable have separate feeds. Draft artifacts do not imply a published
  feed; an unpublished channel is explicitly shown as such in the client.
- No stable release or update-channel advancement is authorized by this audit.
- Windows update signatures are not Authenticode distribution signing.
- macOS signing/notarization of an earlier build is not acceptance of subsequent
  UI or runtime edits. New native builds and human inspection remain necessary.
- The browser tray is a limited preview, not evidence for native menu behavior.
- Window persistence uses the official window-state plugin for main-window
  position/size/maximization only. Monitor-detachment restoration, actual OS close
  events and VoiceOver still need macOS/Windows platform acceptance; browser
  dispatch tests verify the shared content guard, not the native event delivery.
