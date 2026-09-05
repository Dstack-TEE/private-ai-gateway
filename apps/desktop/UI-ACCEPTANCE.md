# Desktop UI Acceptance Audit

Audited September 5, 2026 against `62a1bf5` plus the local corrections below.
Renderer tests use mock runtime data in Chromium; they do not validate AppKit,
WKWebView, native menu actions, or the installed release. A passing build is not
visual acceptance. The previous statement that all 18 requests were complete
was not supported by the verification performed.

| Request | Current evidence | Remaining acceptance |
| --- | --- | --- |
| 1. Connected count only | Renderer counts installed connected agents; no active count | None identified in renderer |
| 2. Same header status | Fixed: PageHeader now uses the same presentation function as Overview, including blocked/error states | Native rendering |
| 3. Last-row spacing | Both status columns add 6px before their last row | Original screenshot intent was not confirmed |
| 4. Wider profile selector | Width is 152px, bounded by its parent | User visual acceptance |
| 5. Verified text plus info | Info appears only for live hardware verification | None identified in renderer |
| 6. Agent name/status alignment | Shared row uses centered flex alignment; regression checks centers | Long names and native font rendering |
| 7. Inactive tray gray, no badge | Single template asset; inactive alpha is 45%; no badge asset | Actual macOS menu-bar appearance in light/dark modes |
| 8. Expanded tray menu | Native code includes endpoint/key copy, profiles, agents, elapsed time | Clipboard, profile reconnect, agent toggles and error handling need native interaction testing; browser menu is stale |
| 9. Proof layouts | Shared proof layout; fixed clipped identity hashes to wrap fully | Native viewport and realistic long evidence |
| 10. Muted inactive/verifying | Follow-up found the neutral CSS rule was missing; now explicitly uses the neutral color token with computed-color assertions | Native rendering |
| 11. Overview mark/spacing | Mark 38px; switch top margin 9px | User visual acceptance |
| 12. Four preview rows/window fit | Both lists capped at four; default window 1052x784; mock content fits | Native fonts, longer agent attention messages |
| 13. About links | Documentation points to the gateway quickstart; GitHub opens its source repository | Desktop-specific onboarding documentation remains limited |
| 14. Unified Settings group | General includes startup, Profiles and Local API; configuration rows clickable | None identified in renderer |
| 15. Equal module spacing | Overview top/inter-row spacing both 28px | Native visual acceptance |
| 16. Nonselectable chrome | Body disables selection; inputs and evidence opt back in | Native selection behavior |
| 17. Agents bot icon | Sidebar and Overview use Bot | None identified in renderer |
| 18. Agents description width | Dedicated page-intro width cap removed | None identified in renderer |

## Corrections In This Audit

- Removed independent PageHeader status wording/tone; blocked state is checked
  across Agents, Usage and Settings in the existing fail-closed regression.
- Removed single-line truncation of privacy identity values; the existing dialog
  regression checks wrapping and horizontal bounds.
- App/Dock icons, branding and native packaging configuration are unchanged.

## Follow-up Corrections

- Added the missing neutral color rule; changing only the state class had not
  changed the rendered headline color.
- Removed the Overview provider endpoint line as requested.
- Preserved the row separator on clickable list rows instead of resetting all
  borders. Browser checks cover both the General and About groups.
- Moved update checks/install actions into the version control in About's first
  row; progress, errors and install confirmation remain available.
- Added manual agent detection to the Agents page header using the existing scan
  path, loading state and error handling.
- Added `core:window:allow-internal-toggle-maximize`. The installed Tauri 2.11.5
  drag script invokes this separately from `start_dragging` on macOS mouseup;
  no custom drag or double-click handler was added. Native behavior still needs
  testing in a newly packaged app.

Verification: TypeScript check and all 18 renderer tests pass. An agent-browser
check at 1100x900 found no Overview content overflow, equal status heading top
coordinates, and no Settings row horizontal overflow. Screenshot capture worked,
but image inspection was unavailable; this is not pixel-level visual acceptance.

## Release Gate

Additional follow-up corrections:

- Brand marks now use direct vector path paint and an even-odd clip for the eye,
  with no tint filter or mask. The macOS mark layer has no material blur,
  translucency, specular effect or shadow. Source logo geometry is preserved.
- The native tray separates status (including elapsed time) from explicit Start,
  Stop and Cancel actions. It no longer presents a checkmark labelled Protected.
- Configuration-only verification does not make the tray choose Stop. Its next
  action starts protection. Native operations use the existing runtime controller
  on a worker thread rather than performing lifecycle work on the menu thread.
- The browser tray no longer draws a switch that the native menu does not have;
  it remains a limited preview, not an acceptance test for native submenus.

Still not release-complete: these version-0.1.0 test packages do not configure the
updater endpoint (the workflow requires an explicit release version). Publishing
an update channel and validating Windows distribution signing are separate from
Apple notarization. No full HIG or VoiceOver compliance claim is justified.

Do not label this audit as full product acceptance or present the previous
download as containing these local corrections. Native tray interactions and
WKWebView dialog appearance still need macOS verification. The browser tray mock
must not be used as evidence for the native menu contract.
