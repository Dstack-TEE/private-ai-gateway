# Private AI Gateway Desktop

Cross-platform Tauri desktop app that turns the bundled `aci serve` verifier
into a local gateway for Codex, Claude Code, OpenCode, Pi, Hermes, OpenClaw, and
Oh My Pi. One Rust
runtime owns policy, persistence, credentials, usage, agent projection, and
process lifecycle. A shared React renderer owns the dense product UI, while
Tauri delegates windows, menus, tray integration, file dialogs, confirmation
dialogs, clipboard, autostart, and application lifecycle to each operating
system.

Profiles, Local API settings, Privacy verification, and Usage proof open as
document-modal AppKit sheets on macOS, without traffic lights or an independent
title bar. Done, Cancel, and Escape dismiss the sheet, including loading/error
states. Windows uses owned windows and Linux uses transient windows. Complex content stays in the shared renderer
so behavior and accessibility do not drift across three platform-specific UI
implementations. Destructive confirmations and file destinations use the
operating system's native dialogs directly.

### Renderer Components

The renderer uses the official shadcn/ui **Base Luma** style with Base UI,
Tailwind CSS 4, Lucide, and the official Neutral light/dark primary palette.
Success uses green-700/green-400, warnings use amber-700/amber-400, and errors
use the destructive token. Overview decoration uses the neutral primary palette.
Protection switches use success when enabled, with warning taking precedence in
development mode; preference and agent switches retain the default theme.
`src/renderer/theme.css` defines the palette and aliases for existing page layouts.
`styles.css` owns product layout, not a second button/switch implementation.
Tailwind Preflight and shadcn's standard CSS are enabled. System selects retain
their browser/platform picker. Do not override component dimensions, radii,
shadows or typography; choose from the official component variants instead.

Component sources in `src/renderer/components/ui` were obtained from the official
`https://ui.shadcn.com/r/styles/base-luma/{component}.json` registry on 2026-09-05.
Local changes are import paths, Lucide icon substitution and an explicit large
switch size for the Overview protection control. Its geometry is 60x28 with
the existing Base UI switch behavior; other switches use the default 44x20.
Sidebar buttons use Luma's default 36px size, not its 56px large variant.
`components.json` configures subsequent
shadcn additions. Use these components for new standard controls; retain semantic
HTML for navigation/list rows and the operating-system APIs for native surfaces.
Product statuses use semantic tokens: success for verified/available/connected, muted for
unknown/inactive, warning for caution states, destructive for errors, and chart tokens for usage.
Primary remains reserved for commands and selection. The default Luma
appearance is not an AppKit emulation: WebView content is still web content.

Shared product compositions live one layer above `components/ui`:

- `controls.tsx`: named icon actions and protection/preference switches.
- `sheet.tsx`: the single modal lifecycle, heading and action layout. It uses
  HTML `showModal()` for focus containment inside native child-window webviews,
  rather than adding a second library focus trap over the platform sheet.
- `settings.tsx`: grouped settings, navigation rows, toggles and labeled fields.

Forms use the official Field components. Local API fields use FieldGroup and
FieldSeparator, with the Luma InputGroup for inline key actions; standalone
option rows use Item outline. Client endpoint previews are omitted from Settings; the Overview help
button opens a native examples sheet with cURL, Python and JavaScript tabs.
Examples use the configured endpoint and discovered models. The local client key
is read when the example sheet opens and embedded in its copyable snippets; copied
snippets are sensitive. Key changes invalidate the displayed code. Provider keys
are never used in examples.

Longer choice lists use Luma Select, including date presets and calendar
month/year navigation; listen addresses use Combobox. Theme uses an icon
ToggleGroup and the update channel uses a two-option ToggleGroup. General
settings rows are 52px with no inter-row gap; descriptive rows grow naturally.
ChoiceSelect shares composition and portal ownership, not a replacement menu
implementation. Menus inside HTML dialogs portal into their owning dialog.
Form scroll regions own horizontal padding so the standard 3px focus ring is
not clipped. Continuous bordered lists keep edge-to-edge hover backgrounds
and use an inset focus ring on full-row actions. Image clipping and actual
scroll viewports remain intact.

Profiles offers native file-picker import/export of a versioned JSON configuration
backup. It contains only profile names, provider IDs and service URLs, not keys,
OAuth accounts, verification state, active selection or security policy. Imports
are limited to 256 KiB and 50 profiles, validate every entry before writing, skip
exact normalized duplicates, allocate new IDs and never overwrite an existing
profile. Imported entries need credentials and fresh verification before use.
Exported configurations may still reveal private service names and endpoints.

About offers a redacted diagnostics export. It uses a strict field allowlist of
build/platform metadata, boolean gateway state and numeric counts. It excludes
raw stderr, error messages, URLs, profile names, paths, request bodies, model IDs
and credentials. Both exports use the existing atomic writer and native file
dialogs; neither starts a gateway or reads the OS credential store.

Network address changes use [`netwatcher` 0.8](https://docs.rs/netwatcher/0.8.0/),
which subscribes to native interface events rather than adding a polling loop.
The backend owns native wake monitoring: IORegisterForSystemPower on macOS,
PowerRegisterSuspendResumeNotification on Windows and login1 PrepareForSleep(false)
on Linux. There is no elapsed-time inference or UI dependency. Registrations are
removed when the backend exits.
Linux reconnects and re-subscribes every five seconds after subscription failure
or stream termination, with cancellation on backend exit. Monitor availability is
visible in Settings and redacted diagnostics. Environments without login1 retain
network recovery but cannot report wake until the service becomes available.
Recovery revokes the old session and restores agent configurations before a fresh
verification. If all non-loopback addresses disappear it waits for an address to
return. Address presence is not a claim of internet reachability; a failed fresh
verification requires user attention, not unlimited retries. Events survive a busy
lifecycle lock or in-progress verification and are revisited by the existing
reconciliation loop without busy-waiting. Manual start/stop, backend exit/install and
successful active-profile changes cancel recovery intent; failed and no-op imports
do not. Loopback services are exempt.
Real sleep/wake, VPN changes and per-platform notification delivery still require
installed-app acceptance tests.

Notifications has its own native dialog with a master switch and gateway,
Local API and response-verification categories. All default to enabled; saved
choices are never reset on launch. Delivery uses the official Tauri notification
plugin from Rust, independent of renderer lifecycle. Each category is limited
to one notification per minute, and foreground faults are not replayed on hide.
Notification text never includes credentials, prompts, endpoints or profile names.
Authorization is checked on launch and focus, but permission warnings appear
only in Notifications. macOS uses UNUserNotificationCenter settings and Windows
uses ToastNotifier.Setting. Enabling the master switch automatically requests
undetermined permission; denied permission offers system settings instead.
macOS notification authorization and banner availability are separate fields;
disabled banners are not reported as denied authorization.
Linux has no portable per-app authorization query, so the dialog reports
that limitation without claiming permission is granted. Focus modes may still
suppress delivery. Installed packages must be tested on each OS; tray state,
update badges and inline errors remain available without notification delivery.

Native dialogs wait for content, font readiness and image decoding before the
presentation handshake. Example dialogs also wait for their local key read.
On macOS presentation has a short transparent preparation stage: the window is
ordered without becoming key, ignores mouse events, then the renderer waits for
visible animation frames before beginning the sheet. This uses public AppKit
APIs, not snapshots or fixed presentation sleeps. Animation frames are not a
cross-process compositor guarantee; transparent-window visibility and first-frame
behavior require installed-app acceptance on supported macOS versions. The
handshake watchdog also removes a stalled transparent window.
Native dialogs receive the saved appearance with their initial state and apply
it in a layout effect, avoiding a temporary System-theme render before the async
preferences read. Recharts is loaded only on Usage; its fixed-height shell stays
synchronous. Snapshot-based presentation gates are not used: a successful snapshot
does not guarantee compositor readiness, and a failed snapshot must not block a
usable dialog. WebKit has no general public first-composited-frame event.
No sleep is used to delay normal presentation, and no hidden window
pool retains credentials. A 20-second failed-handshake deadline cleans up an
unpresented window and reports the failure in the main window. Browser checks
cover readiness ordering; compositor behavior still requires macOS acceptance.
SheetActions provides one shared footer divider.
Disabled controls are reserved for in-flight mutations, missing/invalid inputs,
unavailable data, pagination boundaries and dependent settings. Development OS
changes and deleting a profile during protection use explicit stop-and-confirm
flows. Deletion aborts if stopping fails; backend lifecycle checks remain in
force if a concurrent client starts protection again. Preset service endpoints
are read-only and copyable, not disabled merely because they are preset values.
Proof and privacy scroll content reserve an overlay-scrollbar lane; scrollable
surfaces use scrollbar-gutter while non-scrolling native dialog roots do not.

Oh My Pi's local SVG comes from can1357/oh-my-pi, commit
08db86f87bda871e574eda56de39745523836116, assets/icon.svg (MIT; adjacent LICENSE).
Sidebar navigation uses SidebarMenu, information rows use Item, status labels use
Badge, and errors use Alert/FieldError. Agent detection runs on startup, window
activation and Agents navigation, plus background reconciliation; no refresh button.
Use the default switch size except for the Overview main switch; do not retain legacy CSS aliases for removed radii.

The sidebar update badge is pinned at the bottom and invokes the same confirmed
installation action as Settings. About displays the installed version and update
status/action together on one row. After native confirmation, the existing native
child-window mechanism presents update progress (an attached sheet on macOS, an
owned window on Windows/Linux), with the shared shadcn Progress content. The web
preview uses Dialog. A backend snapshot replays progress and failures to newly
loaded windows; closing is disabled while installation cannot be cancelled.
Failures are dismissible. Settings, Profile and usage actions
share the same Item-based clickable row. Lists use explicit Separator components.

Usage charts use shadcn Chart and Recharts, loaded only on the Usage page.
SQLite supplies per-day/per-model aggregates using the full filter scope,
independently of pagination. The ten largest models by tokens have individual
stacks; additional models are combined as Other without dropping usage. Colors
come from the complete model facet so filtering does not recolor a model.
Input/output are not separate stacks. Empty dates are filled and ranges over 90 days are
aggregated monthly without dropping totals. Chart configuration contains labels
only; Bar colors reference static theme variables to avoid dynamic style tags
under the production CSP. Chart metric views use Tabs. Date filtering uses the
official Calendar/Popover with local-day boundaries, presets and an explicit
Apply/Cancel flow. Query, summary, chart and CSV share the same date bounds.
Usage details use shadcn Table with TanStack Table v9 manual cursor pagination
(20/50/100 rows), the shared outcome/number presentation, and token-detail popovers.
Rows open the same proof dialog as Overview; no page-local sorting or unmeasured
latency/throughput fields are exposed. Calendar, table and chart load lazily.
The generated Calendar forwards its day-button ref to preserve keyboard focus.

Profile saves continue to verify the endpoint/key before persisting or
reconnecting. No separate verified-configuration badge or credential-delete
action is shown. Agent switches show optimistic pending state instead of a
disabled flash, serialize writes per agent, retain the latest requested state,
and roll back on failure. Concurrent scans are coalesced. Detection feedback is
automatic and does not add expanding status text to the Agents toolbar.

Settings offers System (default), Light and Dark appearance, persisted in runtime
preferences. Tauri applies native appearance; renderer windows synchronize through
an appearance event. Cmd+, opens Settings on macOS through the native app menu;
Ctrl+, is also handled by the renderer. Native editing shortcuts retain their
platform roles. Active modal sheets keep their focus rather than being discarded.

Main window position, size and maximized state are persisted by the official
`tauri-plugin-window-state`; transient dialogs, visibility and decorations are
excluded. The plugin checks saved positions against connected monitors and leaves
placement to the OS when the saved monitor is unavailable.

Native close requests and Cmd/Ctrl+W go through the topmost dialog's existing
dismissal guard before destruction. Saving and update installation cannot be
bypassed by an OS close button. Cmd+. uses the same cancel guard. Form dialogs
focus the first enabled editable field; reading dialogs focus their heading.
Closing a nested editor restores focus to its trigger. The fixed desktop sidebar
opts out of shadcn's collapse shortcut; Cmd/Ctrl+B is left untouched.

All renderer windows suppress the WebView navigation context menu and reload
shortcuts (Cmd/Ctrl+R and F5). Text fields and selected text open a Tauri native
editing menu; read-only fields omit Cut/Paste. Undo/Redo menu roles are macOS-only,
as documented by Tauri. Normal text-editing shortcuts remain unchanged. File drops
cannot navigate the embedded browser away from the application.

Blocking Tauri commands use the shared `run_blocking` boundary for filesystem,
SQLite, credential, agent and startup-preference work. Native window presentation
stays on the platform thread. Exit restoration runs in the background and prevents
later configuration changes after successful restoration; failed restoration keeps
the application open. Async verification and listener operations retain their
existing lifecycle locks.

### Publishing Updates

The pipeline uses official Tauri CLI signing/updater artifacts for macOS,
Windows, and Linux DEB/RPM packages, Apple notarytool, and GitHub Actions/CLI. Rust setup/cache actions are
third-party, not GitHub official actions. All external actions are pinned to commit SHAs; checkout does
not persist credentials, and signing/publishing secrets are scoped to their steps.
Release-only npm installs skip lifecycle scripts. PRs and main pushes run CI;
manual dispatch publishes releases. Feed updates serialize separately by channel.
Manifest/channel orchestration and Icon Composer compilation are project scripts,
not replacements for the official signature or updater engines.

The Desktop Tauri workflow defaults `release_channel` to `beta`. Use
`production_macos=true`, a matching `release_version`, and `publish_release=true`
to publish signed updates. Leave publication disabled to create a draft.

| Channel | Version | GitHub release | Feed tag |
| --- | --- | --- | --- |
| beta (default) | `0.1.2-beta.1` | Pre-release | `desktop-updates-beta` |
| stable (explicit) | `0.1.2` | Release | `desktop-updates-stable` |

Each feed hosts its own `latest.json` with macOS, Windows, and installer-specific
Linux DEB/RPM targets. Canonical SemVer, channel, manifest and GitHub Pre-release
metadata must agree. Feed advancement uses
SemVer comparison,
including numeric beta sequence numbers, and never falls back to another channel.
Release tooling uses `node-semver`; clients use the official Tauri updater's
default version comparator, signature verification and installer. No custom
version comparator or prerelease sorting is used. The client only checks that
the returned manifest's channel matches the user's selection.
The shared update-feed workflow is called explicitly after publication, even before
the release-event workflow exists on the default branch, and avoids relying on
events generated by `GITHUB_TOKEN` to trigger another workflow.

Required update configuration is `TAURI_SIGNING_PRIVATE_KEY` (repository Secret)
and `TAURI_UPDATER_PUBLIC_KEY` (repository Variable), independently of Apple
signing credentials. Never rotate these casually: installed clients trust the
embedded public key. Packages without a release version remain non-updating
test builds. Existing 0.1.0 test installations need one manual installation of
an updater-enabled version. The withdrawn 0.1.1 used the retired
`desktop-updates` URL and also needs a manual migration. That URL is not reused.
Settings > Advanced > Update channel selects Stable or Beta and persists locally.
The initial default follows the installed package's version. Switching clears
any pending update and immediately checks the selected channel. Installation
only accepts a newer version from that channel: choosing Stable while running
a newer beta waits for a higher stable version instead of downgrading.
Updates are checked at startup, every six hours, when connectivity returns,
and on window activation if the last check was over fifteen minutes ago.
About reads the installed version locally even when update checks fail; it has
no manual check button, only an install action when a newer version is available.
A confirmed HTTP 404 means that channel has no published feed yet, not that the
app is current. Other failures remain errors and are retried automatically.
Installation
requires confirmation before stopping protection and restoring agent configs.

Keep policy, persistence and verification in their existing runtime/page owners;
shared presentation components receive values and callbacks only. `main.tsx`
creates the React root once, independently of the hot-reloadable renderer.

New/Edit Profile opens a separate child dialog over the Profiles chooser.
Dialog webviews receive a non-secret state snapshot at initialization and are
presented after their first content commit, not as empty windows while IPC loads.
The initial state is consumed synchronously for the first render. Windows that
still need a credential or historical record stay hidden without a transient
Loading/Cancel page; errors remain actionable and dismissible.

macOS tray image updates preserve the template flag atomically; replacing only
the image resets that flag in the underlying tray implementation and can make
the icon disappear against a dark menu bar.
The tray uses a 36px template raster for Tauri's 18pt macOS image, with a 16pt
mark. Protected uses the full template alpha; stopped or verifying uses 45%
alpha, tinted by macOS. There is no status badge. The Dock app icon is independent
and unchanged. The native tray menu offers endpoint/key copying, profile selection,
agent connection checkmarks, and elapsed protection time. Actions use the same
runtime operations as the main window, including profile reconnection and config restoration.

Agent connections are saved preferences, not permanent config rewrites. Only
connected agents under active protection receive gateway settings. Stopping,
verification failure, or stopping the backend restores the owned settings while retaining
the connection choices. Startup recovers unfinished restoration before any
automatic connection. Uninstalled agents stay linked but inactive; deleted
configs are not recreated, and external edits are preserved. Failed restoration
keeps its journal for retry and prevents backend shutdown from silently discarding it.
Closing or quitting only the desktop UI leaves protection and the backend running;
use Stop All and Quit or `pag --yes service stop` to shut down both.
Force-kill and power loss cannot run cleanup; recovery runs on the next launch.

Settings exposes **Open at Login** (the operating system's login item, also
available in the tray) and **Protect on launch** (off by default). Automatic
connection verifies the selected profile before applying any agent config.

Local API settings can be saved during protection. The runtime serializes this
with start/stop and profile changes, restores connected agent configurations,
rebinds and persists the listener, then re-verifies protection. Reconnection
projects the new endpoint into connected, installed agents. A bind or save
failure restores the old listener before attempting to resume protection.
Requests in flight can be interrupted. Saving identical settings is a no-op.

## Application Updates

On macOS, Windows, and installed Linux DEB/RPM builds, the main window checks once at launch; Settings also
supports manual checks. Installation requires confirmation. The official Tauri
updater downloads and verifies the signed archive before the runtime restores
agent configurations and allows installation. Failed download/signature
verification leaves running protection untouched; failed installation leaves
protection stopped. Linux updater manifests use separate
`linux-x86_64-deb` and `linux-x86_64-rpm` entries, and the locked updater invokes
the matching native installer with user authorization. AppImage is not shipped;
existing AppImage users must manually migrate to DEB or RPM.

Update signatures are separate from Apple Developer ID signing/notarization.
Release administrators must provision these repository settings:

- Secret `TAURI_SIGNING_PRIVATE_KEY`, generated with the Tauri signer and retained securely.
- Optional secret `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`, if the key is encrypted.
- Variable `TAURI_UPDATER_PUBLIC_KEY`, containing the matching public key.

Dispatch `desktop-native.yml` with `production_macos=true`, `release_channel`
(`beta` by default), and a matching `release_version`. CI requires signing
settings and creates macOS, Windows, Linux DEB, and Linux RPM signatures plus
`latest.json`.
Publishing requires `publish_release=true` or explicitly publishing the draft.
The selected channel's feed advances only to a newer version. Its public URL is
`releases/download/desktop-updates-beta/latest.json` or
`releases/download/desktop-updates-stable/latest.json`; Actions artifacts are not a feed.
Do not replace an existing version's assets or rotate the signing key casually.

Ordinary test builds without `release_version` keep updates disabled explicitly.
The first updater-enabled app must be installed manually. For local distribution
builds, set `TAURI_UPDATER_PUBLIC_KEY`, `TAURI_UPDATER_ENDPOINT` (HTTPS), and the
Tauri signing secret; the brand overlay enables updater artifacts only when
both public settings are present. No signing secrets are embedded in the app.

The macOS DMG app automatically attempts user-level `pag` registration after it
is launched from a stable location. Mounted disk images and App Translocation are
rejected so they cannot leave a broken command link. It does not request
administrator privileges or edit shell profiles. Settings > Advanced retains
startup errors for retry; removing the command there disables registration on
subsequent launches. No PKG installer is produced.

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
  blocks protection for that launch. Saving connection choices and restoring
  agents do not require a listener. A second instance hands off to the first (the Tauri
  single-instance plugin only focuses the window; the lock decides).
- **Local endpoint** defaults to loopback-only `http://127.0.0.1:4180`. The
  app claims the configured address and port before agent settings are applied.
  The address Combobox lists active interfaces using `if-addrs` and accepts
  manual input; scope-dependent IPv6 link-local addresses are not suggested.
  Non-loopback addresses display a warning and require native confirmation
  on Save. The persisted permission remains enforced by the Rust resolver.
  The local HTTP listener is unencrypted and must not be exposed to the
  internet. Client host changes advertised URLs, not the bind address.
- **AI service** names the remote endpoint in the UI. The protocol calls it an
  [ACI service](../../spec/aci.md), which can serve inference directly or act
  as an aggregator. This label is not a verification verdict; the current
  verification state is displayed separately.
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
- **AI service profile credentials** and any credential a connection
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
- **AI service profiles** are verified before they are saved. A profile
  combines a user-visible name, provider, endpoint, and authentication method.
  A fresh install starts without a profile and opens New Profile as soon as the
  initial state loads; every profile can be deleted, including the last one.
  Settings offers local, self-hosted branding for the Phala and RedPill
  presets plus a custom HTTPS endpoint. New providers or endpoints require a
  new key, so a credential is never silently reused. Profile metadata is
  written atomically and the current API-key authentication model is shaped so
  an OAuth account can be added as another auth kind later. A successful
  `Verify and Save` selects the profile and returns to the chooser. If protection
  was off it stays off; if it was on, saving or switching profiles stops protection,
  restores agent configs, and starts a freshly verified connection. A failed
  verification keeps protection off and does not save the candidate. Selecting an existing
  profile also closes the chooser. The window and native tray both route a
  missing or unavailable current profile back into this same flow. Legacy
  single-service settings are recognized at
  launch, while their credential migrates to the profile entry on first use so
  opening the app does not request credential-store access.
- **Model catalog** is the verified service's `GET /v1/models`, read through
  the sidecar and published atomically with the identity. It is the single
  source of model truth: agents choose from it, the proxy serves it on
  `/v1/models`, and a request whose `model` is not listed is refused before
  it leaves the machine. Models that disappear on a refresh are reported,
  never replaced.
- **Usage history** is written to an owner-only SQLite database in the app
  data directory and has no automatic retention cutoff. Overview shows four
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
  against the catalog. With `--verify-receipts` (always enabled by desktop),
  the sidecar also buffers responses in memory up to 32 MiB and checks their
  receipts before returning any bytes. Failed, missing or unavailable proofs
  return HTTP 502 without the provider response body. SSE framing is preserved,
  but tokens are delivered only after the entire response is verified. Neither
  request nor response buffers are written to disk. Verification has a 600 s
  upper bound; the desktop transport can time out earlier while waiting.
  At most 64 requests are in flight (`429`); upstream connect 5 s,
  idle read 300 s (`504`). Standard hop-by-hop headers plus any named by
  `Connection`, `Proxy-Connection`, the agent credential, and the attribution
  tag are removed in both directions by both proxies. The helper endpoints
  are gated exactly like inference: token scope, verified session, and a
  catalog model (both protocols require `model`).

## Agents

OpenClaw integration targets the native host's default configuration and its
OpenAI Chat Completions provider contract. It adds a separate `openclaw` token
and an executable SecretRef, not the upstream provider key. On Unix, the backend
stages a private user-owned helper at startup; scans verify that it matches the bundled
helper. Windows checks the helper's ACL without changing it. Configuration
edits preserve JSON5 comments and unrelated fields. An explicit model selection
changes only `agents.defaults.model.primary`; existing fallback lists stay intact.

OpenClaw 2026.9.2 was checked offline with a real generated configuration and
exec-secret audit. This is not real inference or a guarantee for older releases.
Remote gateways, non-default profiles, ambiguous legacy paths, `$include`, and
unsafe helper/configuration conflicts are refused rather than rewritten. WSL and
remote hosts do not implicitly share this desktop's loopback endpoint or token.

Connections now remember their absolute config path. Legacy records without
that path, or containing potentially sensitive structured plaintext backups,
require explicit manual recovery instead of guessing a restore target. New
unmanaged same-name provider collisions are refused without exposing nested
secrets in previews or ordinary connection records.

Oh My Pi is a separate integration: it detects `omp` and manages
`~/.omp/agent/models.yml` or `models.yaml` with its own `oh-my-pi` token and
connection record. YAML comments and unrelated providers are preserved. It uses
Chat Completions; Pi keeps its independent Responses configuration. Select the
provider/model in Oh My Pi and restart it after reconnecting, because command
credentials can be cached by the CLI process. Named profiles and pending legacy
JSON migration are refused rather than silently redirected or rewritten.
Official Oh My Pi v18.1.12 was checked offline on Linux for generated configuration
loading, helper success/failure and YAML file priority. Windows/macOS CLI shell
execution and real inference are not claimed by those checks.

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
icon. The tray uses a single local template mark; inactive states reduce its
alpha without changing its silhouette or adding a badge.

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

The persistent backend launches the target-triple-specific bundled `aci`
binary as an external sidecar.
The development command builds debug sidecars; packaged builds compile release
sidecars from this repository. `npm run dist` produces the native bundle for
the current platform. CI builds the same Tauri application as a macOS DMG and
app, a Windows NSIS installer, and Linux DEB and RPM packages. AppImage is
excluded because its temporary mount cannot own a persistent backend after the
UI exits.

Tests sit at the boundaries. `cargo test --manifest-path gateway/Cargo.toml`
covers the proxy (token scope, fail-closed session, revocation gate, and a
relay check proving that each inference path carries method, path, query,
body, status, and streamed bytes through unchanged), the projections
(round-trip per agent, stale revision, restore all), and the catalog.
`npm run test:renderer` first builds the production renderer, then runs
Playwright against the stateful in-page mock. It covers protection start/stop,
agent discovery and reversible config previews, current-session Overview
usage, persistent-history filters and cursor pagination, CSV/clear flows,
proof and local-block semantics, profile management, system confirmation boundaries,
dark/high-contrast/reduced-motion
media, 200% zoom, and
940/720/540/320 widths. UI checks require the explicit `run_ui_tests` manual
workflow input and are disabled on ordinary PR builds. TypeScript, release
manifest tests, and Rust checks still run. Windows package jobs also execute
the shared libraries' tests. NSIS/DEB installation checks exercise the bundled
CLI, backend, ACI and helper without opening the app, then uninstall the package; temporary
credential-store fixtures are separate from real provider credentials.

## Packaging

`npm run dist` builds the release sidecars and runs `tauri build`. Xcode 26 or
newer is required to package the adaptive macOS app icon. The platform bundle
contains the shared renderer, `pag`, the persistent `pag-service`, `aci`, and
the credential helper. The UI and CLI are clients of the same per-user backend;
there is no second GUI process.

`scripts/bundle-sidecars.mjs` builds four executables with `--locked`: `pag`,
`pag-service`, the `aci` verifier, and `private-ai-gateway-helper`, a console
binary from the gateway crate that prints an agent's local token (kept separate
from the GUI app so stdout works on Windows). A release build passes
`DESKTOP_RELEASE_VERSION` to the CLI/backend as `PAG_BUILD_VERSION`; ordinary
builds use the runtime crate version. The desktop gateway and Tauri crates declare
`rust-version = 1.89`, the highest MSRV in their locked dependency graphs
(`aes` 0.9.3: 1.89; `keyring` 4.2: 1.88), and commit their `Cargo.lock` files.
The root `aci` follows the root workspace toolchain. CI tests the gateway,
runtime, renderer, and Tauri backend, then compiles and bundles the same app on
macOS, Windows, and Linux. It also publishes UI-free CLI archives on all three
platforms and CLI-only DEB/RPM packages on Linux. See
[`CLI-DISTRIBUTION.md`](CLI-DISTRIBUTION.md) for installed paths, PATH ownership,
upgrade behavior, and automatic macOS registration on app startup. macOS additionally
launches the packaged app with an isolated home to verify the command link, then
verifies the compiled asset catalog, legacy ICNS fallback, bundle icon name, DMG,
and zipped app bundle.

### macOS distribution signing

Normal CI packages use an explicit ad-hoc identity and are beta artifacts. A
production macOS package must be started manually from the `Desktop Tauri`
workflow with `production_macos` enabled. That path fails closed unless all of
these repository secrets are present:

- `APPLE_CERTIFICATE`: base64-encoded PKCS#12 containing a Developer ID
  Application certificate and its private key
- `APPLE_CERTIFICATE_PASSWORD`: password used when exporting the PKCS#12
- `APPLE_API_ISSUER`: App Store Connect API issuer ID
- `APPLE_API_KEY`: App Store Connect API key ID
- `APPLE_API_PRIVATE_KEY`: complete contents of the matching `AuthKey_*.p8`

The certificate must be created by the Apple Developer team Account Holder
from a CSR whose private key remains with the person exporting the PKCS#12.
The Tauri bundler imports the certificate, infers the signing identity, signs
the nested sidecars and app with hardened runtime, notarizes and staples the
app, then signs the DMG. CI submits and staples the final DMG separately because
it is the downloaded distribution container. The release artifact is uploaded
only after `codesign`, Gatekeeper assessment, and stapler validation all pass.
