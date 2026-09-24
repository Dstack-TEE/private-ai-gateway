# Mac App Store preparation and submission

See [distribution architecture](distribution.md) for the Direct/MAS matrix and
credential design and the [Agent Integrations activation flow](distribution.md#agent-access-and-credentials). This checklist is not a claim of App Review acceptance.

## Review decision

PAP is a free local proxy client that connects to independent Phala and RedPill
cloud accounts. It does not sell or unlock app features. The balance button may
open the selected provider's external billing page in the system browser, but
only when that provider returns `canTopUp` and a validated account scope. The
same account and credits can be used by other clients and the CLI.

This release must not claim the free-companion exception in Guideline 3.1.3(f),
because that exception expressly excludes calls to action for outside purchase.
Guideline 3.1.1(a) currently permits external purchase buttons and links without
an entitlement in United States storefront apps. Therefore the first MAS release
must remain available only in the United States while provider billing is linked.
Before adding another storefront, either remove the billing action there or
complete the applicable StoreKit entitlement and regional compliance review.
There is no IAP implementation.

MAS exposes only the narrow provider-billing action and continues to block the
current general account portals:

| Entry | Current destination | MAS decision |
| --- | --- | --- |
| Phala billing | `https://cloud.phala.com/{workspace}/billing` | Available from an actionable balance |
| RedPill billing | `https://redpill.ai/{organization}/credits` | Available from an actionable balance |
| Get API key | `https://cloud.phala.com/dashboard`, `https://www.redpill.ai/dashboard` | Blocked: general dashboards are not verified purchase-free |
| Manage account | `https://redpill.ai/{organization}` | Blocked: organization portal is not verified purchase-free |

Account connection, account switching and manual key entry remain. Neutral
account/security/key-management links may return only after their actual connected
pages and redirects have been audited. Reviewer notes must identify Phala and
RedPill as external cloud infrastructure providers and describe exactly where the
balance button goes. Verify hosted connection and billing pages before submission;
they are outside this repository's control. Do not silently add StoreKit or rely
on a reader-app entitlement.

## Build and signing prerequisites

A stable release tag runs `Desktop Tauri`, which calls `Desktop Mac App Store`
before publishing the Direct release (see
[Release orchestration](distribution.md#release-orchestration)). The reusable
workflow resolves at the tagged commit, so the MAS and Direct artifacts cannot
drift to different source revisions. The marketing version is the committed
app version. The build number is the `Desktop Tauri` run number.

The `Desktop Mac App Store` worker pins **macos-26** for verification and production
packaging, retaining `universal-apple-darwin`. It does not use `macos-latest` or
Xcode 27 preview. The runner's GA default is Xcode 26.6 as of 2026-09-19; record
the actual Xcode version in the build log.

Configure the protected `mac-app-store` environment using the step-scoped
`Validate App Store signing settings` and `Validate App Store Connect upload
settings` mappings in the [workflow](../../../.github/workflows/desktop-mac-app-store.yml).
Those mappings are the required-setting contract: application and installer
certificate/password pairs, the application provisioning profile, signing
identities, and (only with upload enabled) the three ASC API credentials. No
separate installer provisioning profile is used by productbuild.

Both preflights run before checkout, dependency installation and compilation.
They reject missing/blank values and malformed base64 or ASC identifier/key
formats without printing values. Actual certificate/profile validity is checked
by import and packaging. Certificate secrets are scoped to preflight/import;
upload secrets are scoped to preflight/upload preparation and delivery. Cleanup
runs with `always()`, including failed preflight runs. Keep the ASC app record,
explicit App ID `org.dstack.private-ai-proxy`, team, agreements and applicable
tax/banking details ready; preflight does not verify external account state.

The committed version must be stable, and CFBundleVersion must increase (1–9999,
optionally two further components 0–99). ASC must confirm uniqueness and
ordering; the repository validates syntax only. For a local unsigned build of a
stable version on macOS:

```sh
APPLE_APP_STORE_BUILD_NUMBER=1 npm run dist:app-store -- --bundles app
```

The build number in the example is not an ASC reservation. The dedicated
Tauri overlay disables native updates and produces an app rather than a DMG.
The packaging script checks identifier, Developer Tools category, macOS >=13,
versions, exact executable inventory, both architectures, profile validity and
Keychain group before signing children, app, then installer. The app receives
App Sandbox, network client/server, user-selected read/write, app-scoped bookmark
and profile-derived identity/Keychain entitlements. Children receive only sandbox
and inherit entitlements. No temporary sandbox exception is used. Settings and
credentials live in `config.toml` and `credentials.toml` inside the app
container ([Settings files](configuration.md)); the Keychain group remains only
so the backend can import and delete what 0.1 saved there. Remove it together
with that import (planned for 0.3; see
[Removal in 0.3](configuration.md#removal-in-03)).

Apple does not allow a Mac App Store distribution-signed app to launch before
App Store processing. The workflow therefore smoke-tests a temporary ad-hoc
signed copy of the same Universal MAS binaries with the shared runtime sandbox
entitlements, then signs the untouched build output for distribution. The smoke
copy omits profile-authorized identity and Keychain groups; the packaging script
validates those separately on the final app, and `altool --validate-app` validates
the final pkg before upload. Do not attempt to make the distribution package
locally runnable or weaken its signature to satisfy CI.

The worker uploads a reviewable pkg artifact by default. Explicit upload is
restricted to main or the release tag, and performs App Store validation before
delivery. A stable release always requests upload and waits for it to succeed
before publishing Direct. Running repository checks does not invoke either release path
or upload anything.

## Privacy and export compliance

No speculative `PrivacyInfo.xcprivacy` is supplied. The published required-reason
API enforcement platform list does not currently include macOS; privacy data
collection disclosures / Nutrition Labels apply across platforms.

Repository audit covers the desktop npm lockfile, the unified desktop Cargo lockfile, native
framework wrappers, and the macOS dependency tree. No named Apple required-manifest
SDK was identified in the current shipped dependency inventory. `openssl-probe`
is a certificate-location Rust utility, not the OpenSSL SDK, and is not in the
macOS dependency tree. Rust/JavaScript wrappers and similarly named packages
must not be treated as proof of an embedded listed SDK. Recheck transitive native
code and repackaged SDKs in the actual artifact against Apple's current list.

Before submission:

- Generate the Xcode Archive privacy report from the signed release artifact /
  archive and inspect all embedded frameworks and SDKs. Add a manifest only for
  actual applicable dependency or collection declarations, backed by that audit.
- Reconcile API key storage/use, OAuth identifiers and tokens, inference content,
  usage records, account balance requests, and user-exported diagnostics with
  the privacy policy and ASC Privacy Answers. Distinguish local-only persistence
  from transmission to the user's selected service. Do not label collection
  "none" merely because the app itself has no analytics SDK.
- Audit Info.plist usage descriptions against actual protected-resource access.
  Home access uses NSOpenPanel, not blanket Full Disk Access; no camera,
  microphone, contacts or tracking permission is requested by this feature.
  Validate protected folders and any local-network privacy prompt on real macOS.
- Confirm the current `ITSAppUsesNonExemptEncryption=false` classification with
  the release owner and answer ASC export-compliance questions accordingly.

References: [SDK requirements](https://developer.apple.com/support/third-party-SDK-requirements/),
[App privacy](https://developer.apple.com/app-store/app-privacy-details/),
[Review guidelines](https://developer.apple.com/app-store/review/guidelines/).

## Signed Mac and TestFlight gates

- Verify the submitted pkg signatures, profile, entitlements and arm64/x86_64
  slices. Test both architectures through a development-signed build or
  TestFlight; a Mac App Store distribution-signed app cannot run directly before
  App Store processing.
- Confirm macOS 13+ login registration requires a user toggle, approval-required
  status opens System Settings, disabling unregisters, and login opens quietly.
  Check a Direct upgrade preserves an existing LaunchAgent choice, migrates once,
  stays quiet on failure/pending approval, and clears both registrations on Disable.
- Follow the linked activation flow from both Overview and Agents: their Enable
  buttons share the same action and query. Before Enable, no picker or Home scan
  occurs and Agent connection toggles are disabled. Cancellation stays inactive;
  the supported Agent catalog remains visible before Enable; successful Enable
  replaces its states with actual installed Agents without connecting them.
  Keep toggles disabled until the scan completes. Test wrong folders, symlinked
  Home, relaunch and system reboot, recoverable stale app bookmarks, regeneration
  of the backend bookmark, revoked permission and moved Home. Verify a second
  Enable while the picker is open presents no second picker. Check the inherited
  service can resolve access under its actual signature and every start has a
  matching stop; failed restoration must withdraw Agent token authority without
  touching inaccessible configuration. Disconnect and Reset retain Home access.
- Connect all supported Agents using Home token files without any PAP helper.
  Check 0600/0700 permissions, no provider secrets in Home, disconnect/reconnect
  rotation, config drift, restoration failure and recovery after relaunch.
- Quit and crash the GUI with protection active. Verify the service exits, the
  Local API port closes, and no external Agent restarts the backend.
  Disabling protection must withdraw authority and restore owned configuration.
- Verify the upgrade from 0.1 imports its Keychain entries into
  `credentials.toml` and deletes them, OAuth callback behavior, balance-button eligibility, external
  browser routing to the correct scoped provider account, blocked general account
  portals, and absence of updater network calls.
- Upload to TestFlight, complete beta review as required, and repeat against the
  downloaded App Store-signed artifact. Supply reviewer credentials and precise
  instructions explaining Home access and the local proxy token model.

## Submit and rollback

Complete screenshots, support/privacy URLs, age rating, content rights, pricing
(free), United States-only storefront availability, privacy answers, export
compliance, and reviewer notes in ASC.
Release only after the signed/TestFlight gates pass. Keep the prior release and
its source/version provenance. Stop phased release or remove availability in ASC
if needed; ship a corrected higher build through App Store review. Never activate
the Direct updater or install replacement code as a MAS rollback mechanism.

## Repository verification

Run from `apps/desktop`: focused Agent contract tests; `cargo test --locked
--workspace`; workspace fmt/clippy; runtime and desktop tests with the
`mac-app-store` feature; `npm run check` (brand generation, TypeScript and Agent
Integrations tests); `npm run test:release` (includes MAS package tests); and
`git diff --check`. Both Direct PR verification and the MAS workflow call
`npm run check`; their `apps/desktop/**` path filters cover renderer, TypeScript
and script changes. Use `npm run test:agents` for a focused rerun.

Agent contract tests cover both credential modes, missing helper, quoted Home
paths, restoration, revocation and rotation. Package tests validate manifest/profile
and updater/executable policy, including real plist Date/Data decoding; they do
not validate signatures. Workflow contract tests execute the actual preflight
scripts with synthetic settings, including missing/invalid signing and upload
credentials. macOS CI checks Direct migration code as well as the MAS feature.
Agent Integrations tests exercise inactive refresh, explicit Enable, cancellation,
silent restoration, access loss, shared query publication, request serialization,
connection gates and non-MAS behavior. Runtime access tests check that inactive
integrations cannot detect/configure Agents or issue tokens and that existing
Agent authority is withdrawn.

Record verification results against the exact source revision in CI or the
release audit. Ignored live, OS Keychain, root and subprocess fixtures do not
count as passed coverage. The macOS-only SMAppService and bookmark branches are
not executed by Linux all-features tests. The MAS workflow checks those
branches on macos-26. Signing, native APIs, browser presentation and ASC review
remain separate gates above.
