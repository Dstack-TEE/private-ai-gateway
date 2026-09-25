# Desktop distribution architecture

The desktop shell selects `DistributionCapabilities` once at compile time and
injects the same policy into every native window. Native commands enforce it too.
The shared local runtime and Agent projector remain independent of the remote
Gateway server. Build and submission instructions live in [Mac App Store](mac-app-store.md).

## Difference matrix

| Area | Direct | Mac App Store | Classification |
| --- | --- | --- | --- |
| macOS Open at Login | `SMAppService.mainAppService` | Same | Shared; explicit user action only |
| Windows / Linux startup | Existing auto-launch backend | Not a MAS target | Platform difference, not a distribution capability |
| Minimum macOS | 13.0 | 13.0 | Shared SMAppService requirement |
| Agent discovery, config projection, token issuance / revocation | Shared projector | Same | Shared |
| Home filesystem access | Ordinary Home | NSOpenPanel selection and persistent security-scoped bookmark | Sandbox requirement |
| Agent credentials | Bundled helper / native file reference | Authorized Home token files / native file reference | Sandbox parent boundary |
| Settings and upstream API keys / OAuth secrets | `config.toml` and owner-only `credentials.toml` in the settings directory ([Settings files](configuration.md)) | Same, in the app container's `Config` subdirectory | Shared; never projected into Home |
| App update | Tauri signed background updater | App Store | Channel requirement |
| CLI registration | Available | Disabled | No shared-location code installation in MAS |
| OAuth, manual key entry, account balance | Available | Available | Shared |
| Provider billing from balance | Available | Available when the provider returns `canTopUp` and a scoped account slug | Shared external cloud-account action |
| Account / Get API key web portals | Available | Hidden and native commands denied pending neutral destinations | Free companion review policy |
| Backend after GUI exit | Existing persistent service | Stops on GUI exit, including parent loss | MAS lifecycle requirement |
| Packaging | Platform packages and updater artifacts | Universal app and signed pkg, provisioning profile | Channel requirement |

Capabilities describe only channel policy: `nativeUpdates`, `cliRegistration`,
`accountPortalLinks`, and `sandboxHomeAccess`. Autostart backend,
helper paths, and signing mechanics are not capabilities. Platform/feature guards
remain at native API and process boundaries, not throughout product components.

## Release orchestration

Versions, changelog and tags come from
[release-please](https://github.com/googleapis/release-please)
(`release-please-config.json`, `.release-please-manifest.json`). On every push
to `main` that touches `apps/desktop`, `Desktop release PR` keeps a release PR
open. The PR carries the next version in `package.json`,
`src-tauri/tauri.conf.json`, the workspace `Cargo.toml` (every crate inherits
its `version`) and `Cargo.lock`, plus the new `CHANGELOG.md` section built from
Conventional Commit titles.
Merging the PR tags the merge commit `desktop-v<version>` and creates a draft
GitHub release with that changelog section as its notes.

- **Beta** is the default: versions go `x.y.z-beta.1`, `-beta.2`, and so on
  (release-please `versioning: prerelease`). After a stable release, a `fix`
  proposes `x.y.(z+1)-beta.1` and a `feat` proposes `x.(y+1).0-beta.1`. A
  `feat` on a patch-level beta moves to the next minor version but keeps the
  beta number: `0.2.1-beta.3` becomes `0.3.0-beta.3`. That is release-please's
  prerelease strategy, not a skipped release.
- **Stable**: the prerelease strategy only ever proposes another beta, so
  going from `0.2.0-beta.N` to `0.2.0` needs a commit on `main` whose message
  has the footer `Release-As: 0.2.0`
  ([release-please: Release-As](https://github.com/googleapis/release-please#how-do-i-change-the-version-number)).
  The commit must change a file under `apps/desktop/`, because
  `Desktop release PR` runs only for pushes that do; an empty commit, as in the
  release-please README example, does not start it. The next release PR then
  proposes `0.2.0`. Afterwards, betas continue from the next version.

The tag starts `Desktop release` (`desktop-release.yml`), which runs only for
release tags and calls `Desktop Tauri` (`desktop-native.yml`) at the tagged
commit. The tag must match the committed version and be contained in `main`.
The run verifies and builds all six targets. For a stable version it also calls
`Desktop Mac App Store` in parallel, which signs, validates and uploads the App
Store build. Only stable versions reach the App Store.

The App Store build's CFBundleVersion is `100 + <Desktop release run number>`.
A Mac app's build number must increase with every upload, across versions, and
is at most three integers and 18 characters
([TN2420](https://developer.apple.com/library/archive/technotes/tn2420/_index.html)).
The run number increases only on release tags, and a re-run keeps it. Runs
1–3 were manual runs of the workflow file, and the `0.2.0-beta.1` and
`0.2.0-beta.2` tags used runs 4 and 5. Beta tags use a run number but upload
nothing, so the first App Store upload is `100 +` the run number of the first
stable tag, at least 106. The offset
starts above the hand-numbered builds 1–17, as Xcode Cloud's
[next build number](https://developer.apple.com/documentation/xcode/setting-the-next-build-number-for-xcode-cloud-builds)
does for existing Mac apps.

Once every package and the App Store upload have succeeded, the run:

1. attaches the signed assets, `latest.json` and `SHA256SUMS` to the draft.
   Every file in `SHA256SUMS` gets a signed SLSA build provenance attestation
   (`gh attestation verify <file> --repo Dstack-TEE/private-ai-gateway`);
2. publishes it (stable releases become Latest);
3. advances the updater feeds. The publish is idempotent, so after a transient
   GitHub failure use **Re-run failed jobs**, which also runs the npm publish
   that the failure skipped. Dispatching `desktop-update-feed.yml` on the tag
   instead leaves `publish-npm` skipped in the release run; then also dispatch
   `private-ai-proxy-npm.yml` on the tag with `release_tag` set to the tag and
   `publish` enabled;
4. dispatches the dedicated npm publisher at the release tag and waits for it.
   npm checks the top-level workflow identity for OIDC trusted publishing.

A MAS failure therefore cannot leave a newly public Direct release. A later
Direct failure can leave only an uploaded, unsubmitted App Store build. To
recover, use **Re-run failed jobs** on the tag's `Desktop release` run, never
**Re-run all jobs**, which would upload the same App Store build number again
([Mac App Store](mac-app-store.md#build-and-signing-prerequisites)). Windows
Authenticode remains optional and does not block the release.

### Release GitHub App

Workflows never start from events that `GITHUB_TOKEN` creates. `Desktop release
PR` therefore acts as a GitHub App, so its release PR runs CI and its tag starts
the release build. Until both settings below exist, the workflow fails with a
message naming them.

1. Create a GitHub App owned by the Dstack-TEE organization. Disable its
   webhook. It needs these repository permissions, with nothing else beyond the
   mandatory Metadata read:
   - Contents: read and write (release commits, tags, draft releases);
   - Pull requests: read and write (the release PR);
   - Issues: read and write (release PR labels).
2. Install it only on `Dstack-TEE/private-ai-gateway`.
3. In the `desktop-release` environment, store the App's client ID as the
   variable `DESKTOP_RELEASE_APP_CLIENT_ID` and a private key (the whole `.pem`)
   as the secret `DESKTOP_RELEASE_APP_PRIVATE_KEY`. Keep the environment's
   deployment rules to `main` and `desktop-v*`, so other branches cannot mint
   the token.

To rotate the key:

1. Generate a new private key in the App settings.
2. Replace the secret.
3. Confirm the next `Desktop release PR` run succeeds.
4. Delete the old key in the App settings.

Installation tokens last one hour and are minted per run, so nothing else
expires.

## Updates by installation

Every Direct installation follows the saved update channel (desktop **Update
channel** toggle, or `pap settings set update-channel beta|stable`); without a saved
choice it follows the channel of the running build. Each client reads one
static Tauri manifest, `desktop-updates-<channel>/latest.json`. Stable releases
are published to both feeds (as electron-builder's
`generateUpdatesFilesForAllChannels` does), so beta users also receive a stable
release that is newer than the latest beta. The stable feed never carries a
beta. A feed only advances to a newer SemVer version, and switching channels
never downgrades: a beta build stays installed until the new channel has a newer
release.

Feeds are the GitHub releases `desktop-updates-beta` and `desktop-updates-stable`.
`latest.json` lists every platform under Tauri's `<os>-<arch>` and
`<os>-<arch>-<installer>` keys; its `version` must belong to the feed. Every
artifact is signed with the updater key and verified before installation.

Clients up to 0.1.7-beta.4 read per-platform `latest-<os>-<arch>.json` files
instead and require their `channel` to match the feed. Releases 0.1.7-beta.2 to
0.1.7-beta.4 read both the beta and the stable feed and offer whichever release
is newer; releases up to and including 0.1.7-beta.1 only read their own
channel's feed. Each release keeps writing those files to its own channel's
feed until 0.3 (see [Removal in 0.3](configuration.md#removal-in-03)).

Beta builds that predate per-platform feeds (before 0.1.2-beta.44) read the beta
feed's `latest.json` and accept only a beta. After a stable release is published
to the beta feed, they report "The update does not match the selected channel"
until the next beta is published. This is an accepted consequence of
publishing stable releases to the beta feed.

| Installation | Update key or source | How it updates | Backend and restart |
| --- | --- | --- | --- |
| macOS DMG app | `darwin-<arch>` (`.app.tar.gz`) | In-app: the Tauri updater replaces the app bundle | The owned backend is stopped first; the relaunched app starts the new bundled backend |
| Windows NSIS (current user) | `windows-<arch>` (the updater falls back from `windows-<arch>-nsis`) | In-app: the signed setup runs with `/P /UPDATE /R` | The owned backend is stopped first; the installer hooks stop any remaining backend under the startup lock, keep the user `PATH` entry, and relaunch the app |
| Linux desktop DEB | `linux-<arch>-deb` | In-app: `pkexec dpkg -i` (then zenity/kdialog with `sudo -S`, then `sudo`) | The owned backend is stopped first; the app restarts and starts the new backend |
| Linux desktop RPM | `linux-<arch>-rpm` | In-app: `pkexec rpm -U` with the same fallbacks | As DEB |
| Arch desktop package | Version from `latest.json` | Settings > About shows the release and the steps: quit the app, `private-ai-proxy --yes service stop`, `sudo pacman -U <release package URL>` | The next launch starts the new backend |
| CLI-only DEB | Same | `pap doctor` and the web UI show `private-ai-proxy --yes service stop`, `curl -fLO <URL>`, `sudo apt install ./<file>` | User-driven; the next client start runs the new backend |
| CLI-only RPM | Same | As above with `sudo rpm -U <URL>` | As above |
| CLI-only Arch | Same | As above with `sudo pacman -U <URL>` | As above |
| npm | Same | As above with `npm install --global private-ai-proxy@<version>` | As above |
| Portable archive | Same | As above with the archive URL; extract it into a fresh directory | As above |
| Mac App Store | App Store | App Store | MAS has no feed checks |
| AppImage (legacy) | None | Unsupported; migrate to a native package manually | — |

Package-manager installs never modify themselves: they announce the release and
print exact commands built from the compiled feed location and the validated
version. Linux CLI and Arch packages record their owner in
`/usr/share/private-ai-proxy/package-manager` (`deb`, `rpm` or `pacman`); the
desktop disables in-app installation only when that marker says `pacman`. CLI
packages from releases up to 0.1.7-beta.1 lack the marker and are reported as a
system package without commands.

Native package versions keep prereleases ordered before their stable release:
DEB and RPM `x.y.z~beta.n` (RPM release `1`, no epoch) and Arch `x.y.zbeta.n`.
Tauri writes the SemVer string verbatim, so the package job builds the desktop
DEB, RPM and Arch packages from the payload of Tauri's DEB with
[nfpm](https://nfpm.goreleaser.com/), GoReleaser's packager, as it builds the
CLI packages (`scripts/package-linux.mjs`), then signs the final DEB and RPM for
the updater before the manifest is created. nfpm writes a SemVer prerelease
with a tilde, which sorts before the release in dpkg (Debian Policy 5.6.12) and
rpm (Fedora versioning guidelines). Its archlinux packager drops the
prerelease from `pkgver`, so Arch packages get the pacman form instead, which
sorts a trailing letter segment before the release (`vercmp`). Every earlier package sorts before
`0.2.0~beta.1`, including the RPMs published as `0.1.7-0.beta.n.1` and the
0.1.7-beta.1 desktop RPM published as `0.1.7-beta.1-1`, so they update in place.

The packages do not check for running processes, like the Chrome, VS Code and
Firefox packages: dpkg, rpm and pacman already refuse files that another
package owns, and replacing the executables of a running program is safe on
Linux. A backend keeps running its old build until it stops; the next client
command restarts a backend from another build. Package metadata (name,
maintainer, homepage and the desktop description) comes from the brand
configuration, as Tauri's does, and every entry carries the commit time (or
`SOURCE_DATE_EPOCH`), so builds are reproducible and pacman's file checks match.

Packages up to 0.1.7-beta.n refuse removal while a Private AI Proxy process
other than the updating app runs. Only DEB upgrades met that: dpkg runs the old
package's `prerm upgrade` before unpacking. The DEBs therefore carry one
maintainer script, a `prerm` that succeeds for `failed-upgrade`, which dpkg runs
when the old `prerm` fails (Debian Policy 6.6) and which lets the upgrade
continue. rpm runs the old `%preun` only after the new files are installed, and
pacman runs only the new package's scripts on upgrade.

### Planned removals (0.3)

The packaging compatibility code for installations of 0.1.x (the DEB `prerm`
for `failed-upgrade`, the Windows `.cmd` shim migration) is listed with every
other 0.3 removal in [Removal in 0.3](configuration.md#removal-in-03).

A feed advances only after its release is public and every manifest URL
responds, so a feed never names an unpublished asset. Assets are replaced one
file at a time; a client that reads during the replacement sees the previous
release or a transient "temporarily unavailable" error and retries.

## Agent access and credentials

MAS exposes **Agent Integrations** as an explicitly enabled, app-level module.
Before activation, startup, window focus, and opening Agents only check bookmark
status; neither the renderer nor background reconciliation scans Home or opens a
permission panel. Overview and the Agents page expose the same Enable action,
backed by one shared query and authorization flow. Agent connection toggles on
both pages are disabled while integrations are inactive or Enable is pending;
the connection handler enforces the same gate. After activation, Overview shows
**View all** to open the Agents page.

Before activation, the Agents page still shows the supported Agent catalog with
an **Access required** state; it does not claim that any Agent is installed.

Enable opens the native directory picker directly, initially at the real Home
from the OS user account. It accepts directories only, and both selection and
bookmark restoration compare canonical paths with the current user's actual
Home (including symlink resolution). The explanation covers detection in Agent
configuration folders and configuration of Agents the user chooses to connect,
with revocable local proxy tokens; workspace contents are not read. Cancel
leaves the module inactive and retryable, without a scan, Agent configuration,
token issuance, or error alert. No intermediate webview dialog is created.

After selection, MAS stores a persistent app-scoped security bookmark in the app
container. Before every backend launch or restart, the app resolves that bookmark
and creates a fresh process-shareable bookmark for its owned backend, holding
scoped access only while it does so. The backend resolves the shared bookmark,
starts scoped access and holds it for its lifetime, and immediately
scans and shows the actual installed Agents. Detection derives each Agent's configuration path
from that authorized Home and checks for the Agent's official executable in the
user-owned install directories, including common package-manager and version-manager
shim and managed Node install directories. A configuration folder alone is not treated
as an installation.
MAS cannot inspect arbitrary paths outside the selected Home,
so a CLI installed only in a system-wide location is not reported as installed
by the sandboxed build. Enable does not connect an Agent. Connect/Disconnect
remain independent configuration operations and never open the Home picker.

On subsequent launches and refreshes, the persistent app bookmark restores access
silently; a stale but recoverable bookmark is refreshed while scoped access is
active. The process-shareable bookmark is regenerated before each backend start
because its implicit sandbox extension is not a durable replacement for the
app-scoped bookmark. Unrecoverable access clears the backend bookmark and returns
the module to its inactive state with
**Re-enable Agent Integrations**, without a background panel. The backend stops
scanning and withdraws in-memory Agent token authority. Existing owned files and
recovery records remain for restoration after reauthorization; inaccessible Home
is not modified. The backend retains the resolved security scope for the entire
period in which it scans or updates Agent files, and releases it through RAII.

Disconnect preserves app-level Home authorization. Existing **Reset settings**
stops protection and restores/disconnects managed Agents when access is available;
it retains the bookmark, just as it retains system notification permission. There
is no existing explicit Home revoke control, and no new Disable control is added.
Direct, Windows, and Linux retain ordinary discovery without this module gate.

MAS exports only random, revocable, agent-scoped **local proxy tokens** to
`~/.org.dstack.private-ai-proxy-agents/agent-tokens/` (directories 0700, files
0600). Public Codex model metadata lives alongside these files. Neither provider
API keys nor OAuth credentials are written there. These tokens authorize the
agent's local inference surface, not management RPC or upstream API access. They
do not protect against other programs running as the same OS user.

OpenCode uses its file reference and OpenClaw its `singleValue` file SecretRef.
Codex invokes `/bin/cat` with a separate absolute-path argument; Claude Code,
Pi, Oh My Pi and Hermes use their existing credential-command contracts with a
quoted `/bin/cat` path. No external Agent launches a PAP executable or reads the
app container. The same transaction journal restores owned configuration,
revokes tokens on disconnect/suspend, and rotates them on reconnect. In-memory
proxy authority is withdrawn before restoration; restoration failures remain
retryable. File removal alone is not the entire revocation mechanism.

MAS bundles only the service, which contains the verifier and inherits the
sandboxed application's sandbox. The credential helper remains Direct-only;
MAS neither copies it to the container nor installs it in Home/shared paths.
The private management socket uses the short `pap-ipc/api.sock` path at the
root of the App Container so the Unix socket length limit is respected; it never
uses `/private/tmp`.

## Removed duplication and retained boundaries

The macOS Direct LaunchAgent plugin is removed. A one-time Direct-only upgrade
bridge queries and removes that registration with `auto-launch`'s LaunchAgent
mode, the same crate Windows and Linux use for Open at Login.
The verified tauri-plugin-autostart 2.5.1 contract uses `app.package_info().name`
for both label and `~/Library/LaunchAgents/{name}.plist`, with the canonical
executable followed by `--autostart` in ProgramArguments. No path is guessed.

An existing registration preserves the user's prior Enable choice: startup
registers SMAppService silently and removes the legacy entry only after the new
service reports Enabled. Failure or pending approval retains the old entry and
retries on the next launch without opening Settings or a global error. The UI
continues to reflect the retained registration. Explicit Disable attempts both
native unregister and legacy removal, reporting failure rather than a false
success. Legacy `--autostart` launches remain quiet during this transition.
Successful removal makes later launches a no-op; the bridge and argument support
can be removed when upgrades from plugin-based releases are no longer supported.
MAS and non-macOS production builds exclude the bridge entirely. New login-item
registration otherwise occurs only through the user's toggle.

Updater initialization and native commands check the same policy as the renderer;
MAS has no background feed checks. The MAS overlay also removes updater config
and artifacts. Shared account presentation renders balance as a button when the
provider explicitly permits top-up and returns a validated account scope; it opens
that provider's external billing page in the system browser. General account and
API-key portals remain channel-gated because they are broader, unverified surfaces.

The app-owned [Codex catalog](codex-catalog.md) replaces runtime CLI probing in
both distributions. Its existing pinned baseline and refresh workflow are reused.
