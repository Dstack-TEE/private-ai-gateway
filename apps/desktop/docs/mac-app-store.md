# Mac App Store preparation and submission

See [distribution architecture](distribution.md) for the Direct/MAS matrix and
credential design and the [Agent Integrations activation flow](distribution.md#agent-access-and-credentials). This checklist is not a claim of App Review acceptance.

## Review decision

PAP is a consumer AI/SaaS application, not a reader app. The current MAS design
pursues the free companion route under Guideline 3.1.3(f): users sign in to an
existing service or enter an existing API key. There is no IAP implementation.
Do not claim the multiplatform exception in 3.1.3(b) permits selling digital
services elsewhere without offering the same items through IAP.

MAS hides and rejects Top Up and the current general account portals:

| Entry | Current destination | MAS decision |
| --- | --- | --- |
| Phala Top Up | `https://cloud.phala.com/{workspace}/billing` | Blocked |
| RedPill Top Up | `https://redpill.ai/{organization}/credits` | Blocked |
| Get API key | `https://cloud.phala.com/dashboard`, `https://www.redpill.ai/dashboard` | Blocked: general dashboards are not verified purchase-free |
| Manage account | `https://redpill.ai/{organization}` | Blocked: organization portal is not verified purchase-free |

Login, account switching, manual key entry and read-only balance remain. Neutral
account/security/key-management links may return only after their actual signed-in
pages and redirects have been audited. No Top Up, Buy credits, Upgrade, Pricing,
or other external purchase CTA may appear in the MAS flow, including OAuth pages
and reviewer-visible metadata. Verify hosted login pages before submission; they
are outside this repository's control. If the free companion route does not fit
the final business model, resolve that with App Review/product owners before
shipping; do not silently add StoreKit or rely on a reader-app entitlement.

## Build and signing prerequisites

The `Desktop Mac App Store` workflow pins **macos-26** for verification and
production packaging, retaining `universal-apple-darwin`. It does not use
`macos-latest` or Xcode 27 preview. The runner's GA default is Xcode 26.6 as of
2026-09-19; record the actual Xcode version in the build log.

Configure the protected `mac-app-store` environment with real values:

- Secrets `MAC_APP_STORE_APPLICATION_CERTIFICATE`,
  `MAC_APP_STORE_APPLICATION_CERTIFICATE_PASSWORD`,
  `MAC_APP_STORE_INSTALLER_CERTIFICATE`,
  `MAC_APP_STORE_INSTALLER_CERTIFICATE_PASSWORD`, and
  `MAC_APP_STORE_PROVISIONING_PROFILE` (certificate/profile payloads are base64).
- Variables `MAC_APP_STORE_APPLICATION_IDENTITY` and
  `MAC_APP_STORE_INSTALLER_IDENTITY` matching those certificates.
- For upload: `APPLE_API_KEY`, `APPLE_API_PRIVATE_KEY`, `APPLE_API_ISSUER`.
- An ASC app record and explicit App ID for `org.dstack.private-ai-proxy`, the
  correct team, contracts, tax/banking status where applicable, and a valid
  **Mac App Store Connect distribution** provisioning profile.

Use a stable marketing version and an increasing CFBundleVersion (1–9999,
optionally two further components 0–99). ASC must confirm uniqueness/ordering;
the repository validates syntax only. For a local unsigned build on macOS:

```sh
DESKTOP_RELEASE_CHANNEL=stable DESKTOP_RELEASE_VERSION=1.0.0 \
APPLE_APP_STORE_BUILD_NUMBER=1 npm run dist:app-store -- --bundles app
```

Choose actual release values; the example is not an ASC reservation. The dedicated
Tauri overlay disables native updates and produces an app rather than a DMG.
The packaging script checks identifier, Developer Tools category, macOS >=13,
versions, exact executable inventory, both architectures, profile validity and
Keychain group before signing children, app, then installer. The app receives
App Sandbox, network client/server, user-selected read/write, app-scoped bookmark
and profile-derived identity/Keychain entitlements. Children receive only sandbox
and inherit entitlements. No temporary sandbox exception is used.

The workflow uploads a reviewable pkg artifact by default. Explicit upload is
restricted to main and performs App Store validation before delivery. This task
does not run that workflow or upload anything.

## Privacy and export compliance

No speculative `PrivacyInfo.xcprivacy` is supplied. The published required-reason
API enforcement platform list does not currently include macOS; privacy data
collection disclosures / Nutrition Labels apply across platforms.

Repository audit covers the desktop npm lockfile, both Cargo lockfiles, native
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

- Install the signed pkg and verify all signatures, profile, entitlements and
  arm64/x86_64 slices. Test both architectures; Linux checks cannot prove this.
- Confirm macOS 13+ login registration requires a user toggle, approval-required
  status opens System Settings, disabling unregisters, and login opens quietly.
  Check an upgrade from the old Direct LaunchAgent implementation for duplicates.
- Follow the linked activation flow from both Overview and Agents: their Enable
  buttons share the same action and query. Before Enable, no picker or Home scan
  occurs and Agent connection toggles are disabled. Cancellation stays inactive;
  successful Enable immediately shows installed Agents without connecting them.
  Keep toggles disabled until the scan completes. Test wrong folders, symlinked
  Home, relaunch,
  recoverable stale bookmarks, revoked permission and moved Home. Verify no
  duplicate/blank sheets when native dialogs are already open. Check the inherited
  service can resolve access under its actual signature and every start has a
  matching stop; failed restoration must withdraw Agent token authority without
  touching inaccessible configuration. Disconnect and Reset retain Home access.
- Connect all supported Agents using Home token files without any PAP helper.
  Check 0600/0700 permissions, no provider secrets in Home, disconnect/reconnect
  rotation, config drift, restoration failure and recovery after relaunch.
- Quit and crash the GUI with protection active. Verify service, supervisor and
  verifier exit, local ports close, and no external Agent restarts the backend.
  Disabling protection must withdraw authority and restore owned configuration.
- Verify OAuth callback/Keychain behavior, balance display, purchase-free login
  pages, and absence of purchase/account-portal actions and updater network calls.
- Upload to TestFlight, complete beta review as required, and repeat against the
  downloaded App Store-signed artifact. Supply reviewer credentials and precise
  instructions explaining Home access and the local proxy token model.

## Submit and rollback

Complete screenshots, support/privacy URLs, age rating, content rights, pricing
(free companion), privacy answers, export compliance, and reviewer notes in ASC.
Release only after the signed/TestFlight gates pass. Keep the prior release and
its source/version provenance. Stop phased release or remove availability in ASC
if needed; ship a corrected higher build through App Store review. Never activate
the Direct updater or install replacement code as a MAS rollback mechanism.

## Repository verification

Run from `apps/desktop`: focused Agent contract tests; `cargo test --locked
--workspace --all-features`; workspace and src-tauri fmt/clippy; src-tauri tests
for both feature selections; `npm run check` (brand generation, TypeScript and
Agent Integrations tests); `npm run test:release` (includes MAS package tests);
and `git diff --check`. Both Direct PR verification and the MAS workflow call
`npm run check`; their `apps/desktop/**` path filters cover renderer, TypeScript
and script changes. Use `npm run test:agents` for a focused rerun.

Agent contract tests cover both credential modes, missing helper, quoted Home
paths, restoration, revocation and rotation. Package tests validate manifest/profile
and updater/executable policy, including real plist Date/Data decoding; they do
not validate signatures.
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
