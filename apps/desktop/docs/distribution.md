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

`Desktop stable release` is the standard production entry for a coordinated
desktop release. It validates the stable version, App Store build number, release
summary and `main` ref before any signing work. It then calls the MAS and Direct
workflows through same-repository reusable workflow references. GitHub resolves
those references at the caller commit, so both distributions build the same source
revision with the same marketing version.

The MAS package is signed, validated and uploaded first. Only after that succeeds
does the Direct workflow build all six targets, publish the GitHub release, advance
the updater feed and dispatch the dedicated npm publisher at the immutable release
tag. The parent waits for that workflow so npm remains part of the coordinated
result while using the exact workflow identity authorized for OIDC trusted
publishing. A MAS failure therefore cannot leave a newly public Direct release; a
later Direct failure can leave only an uploaded, unsubmitted App Store build. The
child workflows retain their focused verification and recovery entry points, but
coordinated stable publication uses the top-level workflow. Windows Authenticode
remains optional and does not block the coordinated release.

## Updates by installation

Every Direct installation follows the saved update channel (desktop **Update
channel** toggle, or `pap settings set updateChannel beta|stable`); without a saved
choice it follows the channel of the running build. The beta channel reads both
the beta and the stable feed and offers whichever release is newer, so beta users
also receive stable releases. The stable channel never offers a beta. Switching
channels never downgrades: a beta build stays installed until the new channel has
a newer release. Releases up to and including 0.1.7-beta.1 predate this and only
read their own channel's feed.

Feeds are the GitHub releases `desktop-updates-beta` and `desktop-updates-stable`.
Clients read `latest-<os>-<arch>.json` (for example `latest-linux-x86_64.json`),
whose `version` and `channel` must match the feed. Every artifact is signed with
the updater key and verified before installation.

| Installation | Update key or source | How it updates | Backend and restart |
| --- | --- | --- | --- |
| macOS DMG app | `darwin-<arch>` (`.app.tar.gz`) | In-app: the Tauri updater replaces the app bundle | The owned backend is stopped first; the relaunched app starts the new bundled backend |
| Windows NSIS (current user) | `windows-<arch>` (the updater falls back from `windows-<arch>-nsis`) | In-app: the signed setup runs with `/P /UPDATE /R` | The owned backend is stopped first; the installer hooks stop any remaining backend under the startup lock, keep the user `PATH` entry, and relaunch the app |
| Linux desktop DEB | `linux-<arch>-deb` | In-app: `pkexec dpkg -i` (then zenity/kdialog with `sudo -S`, then `sudo`) | The owned backend is stopped first; maintainer scripts only exempt the updating app's own process tree; the app restarts and starts the new backend |
| Linux desktop RPM | `linux-<arch>-rpm` | In-app: `pkexec rpm -U` with the same fallbacks | As DEB |
| Arch desktop package | Version from `latest-linux-<arch>.json` | Settings > About shows the release and the steps: quit the app, `private-ai-proxy --yes service stop`, `sudo pacman -U <release package URL>` | pacman refuses while the app or backend runs; the next launch starts the new backend |
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
DEB `x.y.z~beta.n`, RPM `x.y.z-0.beta.n.1`, and Arch `x.y.zbeta.n`. Tauri writes
the SemVer string verbatim, so the package job rewrites the desktop DEB and RPM
with `scripts/normalize-linux-packages.mjs` and re-signs the rewritten bytes
before the manifest is created. The rewritten desktop RPM has no epoch, like the
CLI RPM; rpm compares a missing epoch as 0, the value Tauri writes. dpkg installs
the one-time `0.1.7-beta.1` to `0.1.7~beta.2` transition with a downgrade
warning.

The desktop RPM of 0.1.7-beta.1 was published as `0.1.7-beta.1-1`, which rpm
orders after every later 0.1.7 package, so its in-app update reports that a newer
package is already installed. Anyone who installed that RPM makes one manual
switch: quit the app, run `private-ai-proxy --yes service stop` as the signed-in
user, then install the current desktop RPM from the release page with rpm's
downgrade flag:

```sh
sudo rpm -Uvh --oldpackage https://github.com/Dstack-TEE/private-ai-gateway/releases/download/desktop-v<version>/private-ai-proxy-<version>-linux-<x64|arm64>.rpm
```

`sudo dnf install <same URL>` works too, because dnf 5 accepts the downgrade.
In-app updates work again after that. Packages from 0.1.8 onward sort after
`0.1.7-beta.1-1` and install without this step.

A feed advances only after its release is public and every manifest URL
responds, so a feed never names an unpublished asset. Assets are replaced one
file at a time; a client that reads during the replacement sees the previous
release or a transient "not published" state and retries.

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
and creates a fresh process-shareable bookmark for its owned backend. The backend
resolves the shared bookmark, retains that access for its lifetime, and immediately
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
The private management socket uses the short `pap-ipc/backend.sock` path at the
root of the App Container so the Unix socket length limit is respected; it never
uses `/private/tmp`.

## Removed duplication and retained boundaries

The macOS Direct LaunchAgent plugin is removed. A one-time Direct-only upgrade
bridge reuses its pinned `auto-launch` 0.5.0 public query/disable implementation.
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
