# Desktop Acceptance Audit

Baseline: `9e528c6`, plus the fixes documented below. This is an evidence ledger,
not a claim of complete product, platform or accessibility certification.
Renderer tests exercise the real components against a mock bridge; gateway and
runtime tests exercise filesystem, SQLite, local HTTP and policy behavior.

## Confirmed Corrections

1. Periodic Agent reconciliation previously republished unchanged token sets and
   cancelled admitted requests. Identical sets now leave the credential epoch
   and cancellation gate untouched. Actual revocations still cancel delivery.
2. Agent list scans and client-key rotation now use the same policy lock as
   connect/disconnect, preventing stale token snapshots from restoring access.
3. Client-key rotation retains revoke-first behavior on storage failure. Other
   agent credentials remain unchanged, but the confirmation now correctly warns
   that in-flight requests can be interrupted by a credential mutation.
4. Configuration-only verification disables the protection switch instead of
   offering cancellation that the backend lifecycle transaction cannot perform.
5. A native editor for a deleted/nonexistent profile displays a dismissible error
   instead of silently becoming a New Profile form.
6. Failed usage queries clear stale results and show unknown totals, not results
   from previous filters. Selected agent/model labels survive empty facets after
   clearing history. Charts do not invent zero usage when a query failed.

## Workflow Coverage

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

## Verification

- Renderer: 22 tests passed.
- Gateway: 45 tests passed; one OS keyring integration test intentionally ignored.
- Runtime: 18 tests passed.
- Release tooling: 4 tests passed.
- No provider secrets or production traffic were used for these checks.
- The paused-delivery regression uses notifications, not timing guesses, to
  exercise a reconciliation between admission and upstream send.

## Release Boundaries

- The official shadcn Luma components and approved dstack primary colors remain
  unchanged. Product-specific layouts compose those primitives.
- Profile selection is a dialog workflow, not an inline menu/Select replacement.
- Beta and stable have separate feeds. Draft artifacts do not imply a published
  feed; an unpublished channel is explicitly shown as such in the client.
- No stable release or update-channel advancement is authorized by this audit.
- Windows update signatures are not Authenticode distribution signing.
- macOS signing/notarization of an earlier build is not acceptance of subsequent
  UI or runtime edits. New native builds and human inspection remain necessary.
- The browser tray is a limited preview, not evidence for native menu behavior.
