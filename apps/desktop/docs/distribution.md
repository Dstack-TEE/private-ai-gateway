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
| Upstream API keys and OAuth secrets | OS credential store | OS credential store | Shared; never projected into Home |
| App update | Tauri signed background updater | App Store | Channel requirement |
| CLI registration | Available | Disabled | No shared-location code installation in MAS |
| OAuth, manual key entry, account balance | Available | Available | Shared |
| Top Up | Available | Hidden and native command denied | Free companion review policy |
| Account / Get API key web portals | Available | Hidden and native commands denied pending neutral destinations | Free companion review policy |
| Backend after GUI exit | Existing persistent service | Stops on GUI exit, including parent loss | MAS lifecycle requirement |
| Packaging | Platform packages and updater artifacts | Universal app and signed pkg, provisioning profile | Channel requirement |

Capabilities describe only channel policy: `nativeUpdates`, `cliRegistration`,
`topUpLinks`, `accountPortalLinks`, and `sandboxHomeAccess`. Autostart backend,
helper paths, and signing mechanics are not capabilities. Platform/feature guards
remain at native API and process boundaries, not throughout product components.

## Agent access and credentials

MAS exposes **Agent Integrations** as an explicitly enabled, app-level module.
Before activation, startup, window focus, and opening Agents only check bookmark
status; neither the renderer nor background reconciliation scans Home or opens a
permission panel. Overview and the Agents page expose the same Enable action,
backed by one shared query and authorization flow. Agent connection toggles on
both pages are disabled while integrations are inactive or Enable is pending;
the connection handler enforces the same gate. After activation, Overview shows
**View all** to open the Agents page.

Enable opens the native directory picker directly, initially at the real Home
from the OS user account. It accepts directories only, and both selection and
bookmark restoration compare canonical paths with the current user's actual
Home (including symlink resolution). The explanation covers detection in Agent
configuration folders and configuration of Agents the user chooses to connect,
with revocable local proxy tokens; workspace contents are not read. Cancel
leaves the module inactive and retryable, without a scan, Agent configuration,
token issuance, or error alert. No intermediate webview dialog is created.

After selection, MAS stores a security-scoped bookmark in the app container,
restarts its owned backend to acquire the scope, and immediately scans and shows
installed Agents. Enable does not connect an Agent. Connect/Disconnect remain
independent configuration operations and never open the Home picker.

On subsequent launches and refreshes, a valid bookmark restores access silently;
a stale but recoverable bookmark is refreshed and persisted while scoped access
is active. Unrecoverable access returns the module to its inactive state with
**Re-enable Agent Integrations**, without a background panel. The backend stops
scanning and withdraws in-memory Agent token authority. Existing owned files and
recovery records remain for restoration after reauthorization; inaccessible Home
is not modified. Security-scoped access is balanced through RAII.

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

MAS bundles only the verifier and service. Both are children of the sandboxed
application and inherit its sandbox. The credential helper remains Direct-only;
MAS neither copies it to the container nor installs it in Home/shared paths.

## Removed duplication and retained boundaries

The macOS Direct LaunchAgent plugin and dependency are removed. Startup only
reads login-item status; registration occurs through the user's toggle. Existing
legacy LaunchAgents are not silently migrated or enabled: upgrade testing must
check for old registrations and instruct affected users to disable them before
re-enabling Open at Login.

Updater initialization and native commands check the same policy as the renderer;
MAS has no background feed checks. The MAS overlay also removes updater config
and artifacts. Shared account presentation keeps balance visible without a
purchase action. MAS does not invent an alternative billing-neutral URL.

The app-owned [Codex catalog](codex-catalog.md) replaces runtime CLI probing in
both distributions. Its existing pinned baseline and refresh workflow are reused.
