# Settings files

Private AI Proxy keeps every user setting in two TOML files in one settings
directory, which holds nothing else:

| File | Holds | Permissions |
| --- | --- | --- |
| `config.toml` | Profiles (without keys), the active profile, the Local API and web UI listeners, appearance, notifications, update channel, Protect on launch (`connect-on-launch`), command registration | Owner-only when created; never holds a secret |
| `credentials.toml` | The user's credentials: provider API keys (manual and account sign-in) and the web UI password | Always owner-only: `0600` on macOS and Linux, a protected owner-only DACL on Windows |
| `config.schema.json` | The JSON Schema of `config.toml`, rewritten by each version | Generated |

The layout follows established tools: one `config.toml` like Cargo
(`~/.cargo/config.toml`) and Codex (`~/.codex/config.toml`), with named
`[profiles.<id>]` tables selected by `active-profile` as in Codex; secrets in a
separate owner-only `credentials.toml` like Cargo's `credentials.toml` and the
AWS CLI's `credentials`. Keys are kebab-case, like
[Cargo's configuration](https://doc.rust-lang.org/cargo/reference/config.html),
[`Tauri.toml`](https://v2.tauri.app/develop/configuration-files/) and
[Helix](https://docs.helix-editor.com/configuration.html).
[`pap settings`](cli.md#settings) names each key by its dotted path in the file,
such as `web-ui.port`.

```toml
#:schema ./config.schema.json
active-profile = "work"
appearance = "dark"

[local-api]
port = 4180

[web-ui]
enabled = true
listen-address = "127.0.0.1"

[profiles.work]
name = "Work"
provider = "custom"
remote-url = "https://gateway.example"
```

```toml
# credentials.toml
[profiles.work]
api-key = "sk-..."

[web-ui]
password = "..."
```

The service generates the web UI password when none is set; see
[Web UI](cli.md#web-ui). A `password-hash` in its place comes from an earlier
version and keeps signing in until a new password replaces it.

Usage history, agent connection records, agent tokens, caches, locks and logs
are state, not settings; they stay in the app data directory. So does
`local-state.json` (owner-only), which holds secrets that describe this device
rather than the user: the values a connected agent's credential fields held
before the connection took them over (put back on disconnect) and replaced
account keys this device still has to revoke. Like `agent-connections.json`
beside it, it must not be copied to another device.

## Locations

| Platform | Settings directory | App data (state) |
| --- | --- | --- |
| Linux | `$XDG_CONFIG_HOME/private-ai-proxy` (default `~/.config/private-ai-proxy`) | `$XDG_DATA_HOME/org.dstack.private-ai-proxy` (default `~/.local/share/org.dstack.private-ai-proxy`) |
| macOS (direct download) | `~/.config/private-ai-proxy` | `~/Library/Application Support/org.dstack.private-ai-proxy` |
| macOS (Mac App Store) | `~/Library/Containers/org.dstack.private-ai-proxy/Data/Library/Application Support/org.dstack.private-ai-proxy/Config` | The same path without `Config` |
| Windows | `%USERPROFILE%\.config\private-ai-proxy` | `%APPDATA%\org.dstack.private-ai-proxy` |

The settings directory is always separate from state, so it can be synced as a
whole. It is `~/.config/private-ai-proxy` on every platform, the default of
the [XDG base directories](https://specifications.freedesktop.org/basedir/latest/),
so one dotfiles setup works everywhere; command-line tools such as GitHub CLI
(`~/.config/gh`) and starship (`~/.config/starship.toml`) use `~/.config` on
macOS and Windows too. Only Linux honours `XDG_CONFIG_HOME` (and
`XDG_DATA_HOME`); as the specification requires, a relative value is ignored.
On macOS and Windows an app started from Finder, the Dock or the Start menu
does not see a shell's environment, so honouring the variable there would give
the backend a different settings directory depending on how it was started.
App data follows each platform's convention (Tauri's `app_data_dir`). The Mac
App Store build is sandboxed and cannot write the real home directory, so its
settings stay in the `Config` subdirectory of its container's app directory.
`pap settings show` prints the paths in use.

On Windows the settings directory is in the user profile, not in the roaming
`%APPDATA%`: roaming profiles and folder redirection of AppData no longer carry
the settings to other machines (sync the directory instead; see
[Syncing between devices](#syncing-between-devices)). Uninstalling with the
installer's option to delete the app data removes `%APPDATA%` and
`%LOCALAPPDATA%` data only; delete `%USERPROFILE%\.config\private-ai-proxy`
yourself to remove the settings and the credentials in `credentials.toml`.
Likewise, on macOS and Linux removing the app leaves `~/.config/private-ai-proxy`.

Overrides follow the existing `PRIVATE_AI_PROXY_*` variables:

- `PRIVATE_AI_PROXY_CONFIG_DIR`: exact settings directory (absolute path).
- `PRIVATE_AI_PROXY_DATA_DIR`: exact app data directory; without a settings
  override the settings live in its `Config` subdirectory (the Mac App Store
  build sets it to its container).
- `PRIVATE_AI_PROXY_HOME`: test home; data in `<home>/.private-ai-proxy`,
  settings in `<home>/.private-ai-proxy/Config`.

### Moving from the earlier macOS and Windows location

Up to 0.2.0-beta.8, the direct macOS and Windows builds kept the settings in
the `Config` subdirectory of the app data directory
(`~/Library/Application Support/org.dstack.private-ai-proxy/Config`,
`%APPDATA%\org.dstack.private-ai-proxy\Config`). On start, before it reads
the settings, the backend moves `config.toml` and `credentials.toml` from there
to the settings directory above (`core/src/relocation.rs`); the service log
records each move. State stays in the app data directory. Until the backend
has moved them, the desktop app and the CLI read the settings where they are,
so nothing shows defaults in between.

- Both old files are read before anything is placed. Contents, comments and
  permissions are kept: on the same volume each file is hard-linked into
  place, so it is the same file (with the owner-only `0600` or DACL of
  `credentials.toml`); otherwise it is copied, synced to disk and made
  owner-only for `credentials.toml`. A symlinked `config.toml` becomes a link
  to the same file; on Windows creating it needs Developer Mode or the "Create
  symbolic links" privilege, and without it the move fails as described below.
- Nothing in the settings directory is overwritten. If it already has a file
  with other content, that file is used and the old one is kept. Settings
  reports the old file when the backend starts, again only if the old file
  changes (an earlier version wrote it), and `pap doctor` reports it for as
  long as it is there, with or without the backend. A kept old file is never
  brought back, also not after you delete the new one.
- The old files are removed only after every file is in place,
  `credentials.toml` last, so an interrupted move leaves each file in at least
  one place and the next start finishes it. The old `Config` directory is
  removed once it is empty.
- A settings directory that is a link to the old one, or the reverse, counts
  as moved: nothing is removed from either. A symlinked old directory that
  points elsewhere is left in place and reported.
- If a file cannot be read or moved, nothing placed in that start remains, the
  settings are used from the old directory, the error is shown in Settings and
  `pap doctor`, and the move is retried on the next start.

Linux, the Mac App Store build and the overrides above keep their location;
nothing moves there.

## Editing

The desktop app, the web UI and `pap settings set` all change settings through
the backend service, which edits the files in place with `toml_edit`, as
`cargo add` edits `Cargo.toml`: only keys whose values changed are rewritten,
so comments, key order and formatting survive. A new `config.toml` starts with
a comment header and a `#:schema ./config.schema.json` directive, so editors
that use taplo (Even Better TOML in VS Code, Zed, Helix and others) complete
and validate keys. `pap settings schema` prints the same schema.

You can also edit either file by hand. The service watches the settings
directory (debounced, like Alacritty and Zed watch theirs) and applies saved
edits immediately, the same way the matching command would: a changed Local
API listener rebinds, a changed active profile, service URL, policy or active
key restarts protection, web UI changes reopen its listener, and the desktop
app reapplies appearance and notification preferences.

A key the app does not know (a typo, or a key from a newer version) is
ignored and reported as a warning with its position
(`config.toml:4:17: web-ui.listenAddress: unknown key, ignored`) in Settings,
`pap settings show`, `pap status` and `pap doctor`; the rest of the file
applies. This is Cargo's "unused config key" warning, collected the same way
(with `serde_ignored`), and matches Alacritty and VS Code, which also warn about
unknown settings instead of rejecting the file.

An edit that does not parse or validate is not applied: the previous settings
stay in effect, and the error, with file, line and column
(`config.toml:3:8: invalid type: string "x", expected u16`), is shown in
Settings, `pap settings show`, `pap status` and `pap doctor`. Errors about
`credentials.toml` name the position and key but never quote a value. While a
file is invalid, the app refuses to write it rather than replacing your edit;
fix and save it again. If a file is already invalid when the service starts,
there are no previous settings to keep, so it starts with the defaults for that
file (for `config.toml`: no profiles, default listeners) and shows the error,
as Alacritty and VS Code do with an invalid settings file; saving a fixed file
applies it immediately.

A write reads the file immediately before changing it and replaces it
atomically only if it still holds what was read. If you save the file in that
instant, the app's change fails with a retryable error instead of discarding
your edit, and your edit is applied by the watcher.

The app writes files atomically. A `config.toml` that links elsewhere (as GNU
Stow or chezmoi link dotfiles) is saved to the link's target with the target's
permissions, so the link stays; a read-only target is reported instead. The
watcher watches the settings directory only, so an edit made to the
target itself applies when the service next starts. The app refuses to replace
a symlinked `credentials.toml`; sync the directory rather than linking it.

## Appearance

`appearance` is `system`, `light` or `dark`. The desktop app opens its window
in the saved appearance; the web UI applies it once you sign in (the sign-in
page follows the browser).

On Linux, `system` follows GTK's light or dark preference, which WebKitGTK
uses and which does not always track the desktop's dark style (for example
GNOME's Style setting). Choose `light` or `dark` for a fixed appearance.

## Syncing between devices

The settings directory contains only settings, so it can be synced as a whole
(a dotfiles repository, Syncthing, a cloud folder) on every platform.

- `config.toml` is always safe to sync. Listener addresses and ports apply to
  every synced device.
- `credentials.toml` holds only the user's credentials (API keys, the web UI
  password), so it is safe to sync with `config.toml`, but it is
  plaintext: sync it only through storage you trust with those secrets.
  Otherwise leave it out and enter keys on each device.
- `config.schema.json` is generated; each version rewrites it on start.
- Never sync the app data directory: it is this device's state, including
  `local-state.json`.

## Security

`credentials.toml` is plaintext protected by file permissions, like Cargo's and
the AWS CLI's credential files: owner-only (`0600`) in an owner-only (`0700`)
directory on macOS and Linux. On Windows every write gives it a protected DACL
granting access only to you, LocalSystem and Administrators (what
`icacls /inheritance:r` sets and OpenSSH for Windows requires of private keys),
so it stays owner-only even when `PRIVATE_AI_PROXY_CONFIG_DIR` points outside
your profile. Every write replaces the file with one that is owner-only before
it moves into place, whatever the permissions of the file it replaces, so
there is no moment when another user can read it; `pap doctor` warns when
another user can read the file, on every platform. `local-state.json` in the
app data directory gets the same treatment, and so do agent tokens, which are
owner-only from the moment they are created. Neither file is ever shown by the app:
`settings show`, `status`, diagnostics and logs omit their contents.
Anyone who can read your files as you can read it, which is also true of an
unlocked OS keychain for a process running as you.

## Upgrade notes for 0.2.0

- **Credentials leave the OS keychain.** API keys move from the macOS
  Keychain, Windows Credential Manager or Secret Service to
  `~/.config/private-ai-proxy/credentials.toml` (the Mac App Store build keeps
  it in its container; see [Locations](#locations)), a plaintext file readable
  only by you (`0600`). The keychain entries are deleted once the import has
  been verified. Do not sync that file anywhere public, such as a public
  dotfiles repository. The import is tried once: if the keychain is
  unavailable or its prompt is denied, sign in again or re-enter the key of
  any profile that shows none. Only if the app cannot write its own settings
  files during the import does it try again, and may prompt again, on the
  next start.
- **Receipt audits report; they do not block.** Responses stream to your
  agent as they arrive. Each signed receipt is fetched and audited after the
  response was delivered, and a failed audit is reported in Usage; it cannot
  retract bytes the agent already received.
- **The production OS check relies on the service's own report.** It
  compares the OS image hash that the service records in its RTMR3 event log
  with a reviewed allowlist. It does not rebuild the MRTD and RTMR0-2 boot
  measurements from the OS image, so it does not prove on its own which
  image booted. For that assurance, run a dstack verifier over the same quote
  (see the [CLI README](../cli/README.md)).
- **Verification failures fail closed.** When verification fails or is
  blocked, connected agents stay pointed at the Local API, which refuses
  their requests until protection is verified again; they never fall back to
  their original provider. A Local API address change rewrites them to the
  new address, unless the backend has not verified a catalog since it
  started; then they are restored until verification projects them again.
  Stopping protection, Stop All and Quit, `pap service stop`, Reset settings,
  or disconnecting an agent restores its own configuration. Quitting the app
  from the tray or menu leaves the backend running and the agents pointed at
  it. A backend that is not running cannot restore agents, so before
  uninstalling, stop it while it runs (`pap service start`, then
  `pap service stop`).

## Upgrading from 0.1

0.1 stored settings in `confidential-ai.json`, `local-api.json` and
`preferences.json` in the app data directory, and API keys, account keys and
agent restore values in the OS credential store (macOS Keychain, Windows
Credential Manager, Secret Service). 0.2 imports this device's copy in two
steps (`runtime/src/settings/legacy.rs`):

1. **Settings**, while the backend starts: the old files become `config.toml`
   and the web UI password hash goes to `credentials.toml`. The old files then
   move to `migrated-0.1/` in the app data directory, which records the step.
2. **Saved credentials**, once the backend is listening, so a slow or
   prompting credential store never delays startup: every credential store
   entry the old files and `agent-connections.json` reference goes to
   `credentials.toml` (API keys) or `local-state.json` (agent restore values,
   pending key revocations). Both are synced to disk and read back, and only
   then are the entries deleted from the store, including any whose value the
   files already hold from an interrupted earlier run, so no plaintext copy
   stays behind. An entry whose value differs from the file (for example a
   key you replaced since) is left alone.
   `migrated-0.1/import-complete` records the step once every entry is
   deleted. Each store operation may take at most 60 seconds, which leaves
   time to answer a macOS Keychain prompt. If the store itself fails,
   `migrated-0.1/import-abandoned` records that with the reason and the store
   is never asked again, so a denied Keychain prompt does not come back on
   every start. A problem with this app's own files (for example a damaged
   `local-state.json`) leaves the step pending for the next start instead.
   While the step runs, disconnecting an agent whose original key has not
   been imported yet fails with the same "agent credential could not be
   restored" error 0.1 gave while the credential store was unavailable,
   instead of dropping that key. Once the step is recorded, a disconnect
   restores what was imported and leaves a key that was not unset.

Progress is recorded in the app data directory, never by the existence of
`config.toml`, because the settings directory may have been synced from
another device that upgraded first. Values already in the files win:

- `config.toml`: if it exists, its settings stay as they are; only this
  device's 0.1 profiles whose IDs it lacks are added. Otherwise it is created
  from the 0.1 settings. Settings not taken over are listed in a notice and
  kept in `migrated-0.1/`.
- `credentials.toml`: an existing API key, password or password hash stays.
  A 0.1 API key is imported only for a profile that has none and uses the
  same service URL and the same sign-in (a manual key, or the same provider
  account) as this device's 0.1 profile of that ID, so a key never lands in
  another account's profile.
- `local-state.json`: always this device's; existing entries stay.

A step 1 that fails writes nothing that records it: the old files and the
credential store entries stay where they are, the error stays in Settings,
`pap status` and `pap doctor`, and the step runs again on the next start. While
step 1 has not succeeded, settings cannot be changed, so nothing can be saved
that the import would then have to merge with. If step 2 fails because the
credential store is locked, unavailable (for example Linux without a Secret
Service) or a macOS prompt is denied or left unanswered, the settings are
still in effect, Settings and
`pap doctor` name the profiles whose API key to re-enter once, and those
profiles show no saved key until you sign in again or enter one. The entries
stay in the credential store; remove them there if you like.

The backup contains no API keys; `preferences.json` contains the web UI
password hash. Delete `migrated-0.1/` once you are satisfied (keep
`import-complete` or `import-abandoned`, or the next start looks for
credentials to import once more).

## Removal in 0.3

The single list of compatibility code to remove in 0.3, once upgrading from
0.1 directly to 0.3 is no longer supported. Each item exists only for
installations of 0.1 or of 0.2 prereleases, or for command-line options
deprecated in 0.2; other documents link here instead of keeping their own
lists.

- `runtime/src/settings/legacy.rs` (with its tests) and the `keyring`
  dependency, the only remaining users of the OS credential store.
- The import's hooks outside that module:
  - `LocalState::importing`/`set_importing` and the check in its
    `SecretStore::get` (`runtime/src/local_state.rs`);
  - `DesktopRuntime::data_dir`, the `set_importing` call in `launch` and
    `import_legacy_secrets` (`runtime/src/controller.rs`,
    `runtime/src/controller/settings_files.rs`), and its call at the start of
    the server's startup task (`runtime/src/server.rs`);
  - `Settings::import_error`, `import_ready`, `add_import_notices`,
    `Applied::import_notices` and `IMPORT_PENDING` with its check in
    `Settings::writable` (`runtime/src/settings.rs`).
- The Keychain access group entitlement of the Mac App Store build (see
  [Mac App Store](mac-app-store.md)), kept only so the backend can import and
  delete what 0.1 saved in the Keychain.
- The 0.1 flat `pap settings set` names (`webUi`, `connectOnLaunch`, …): the
  hidden `alias`es and `SettingsKeyArg::deprecation` in `cli/manage/args.rs`,
  and the hidden `notifications` key that takes a JSON object
  (`SettingsKey::Notifications`, its branch in `cli/manage/mod.rs`).
- The hidden `pap send --api-key` option (`cli/args.rs`) and its warning in
  `cli/send.rs`; `--api-key-stdin` and `ACI_API_KEY` replace it.
- The DEB `prerm` for `failed-upgrade` (`src-tauri/installer/deb-prerm.sh`),
  which lets an upgrade continue when the `prerm` of an installed package up
  to 0.1.7-beta refuses it because Private AI Proxy is running.
- `pap cli install` replacing the Windows `.cmd` shims earlier releases wrote
  (`LEGACY_SCRIPT` in `cli/manage/install.rs`).
- The move of the settings from the earlier macOS and Windows location
  (`core/src/relocation.rs`, `paths::legacy_config_dir`, the calls in
  `DesktopRuntime::launch`, `config::config_path`, `config::credentials_path`
  and the `legacySettingsDirectory` warning of `pap doctor`), for
  installations of 0.2 prereleases.
- `src-tauri/src/autostart/migration.rs`, the bridge from the
  tauri-plugin-autostart login item.
- The per-platform update manifests `latest-<os>-<arch>.json` that clients up
  to 0.1.7-beta.4 read (see [Updates by installation](distribution.md#updates-by-installation)):
  the legacy branch of `updateFeeds` in `scripts/update-feeds.mjs`. Delete the
  files from the `desktop-updates-beta` and `desktop-updates-stable` releases
  afterwards; later clients read only `latest.json`.
- The client that stops a 0.1.4 to 0.2 beta backend over its NDJSON
  protocol, so an update can replace a backend still running the old build:
  `core/src/client/legacy.rs`, its calls in `core/src/client.rs`
  (`is_running`, `version`, `ensure_service`, `shutdown_owned`),
  `transport::legacy_endpoint_path` with `LEGACY_SOCKET_FILE`, and the test
  `a_legacy_backend_is_stopped_over_its_own_protocol`
  (`cli/tests/management.rs`).

Not on this list: the `aci` command stays a documented legacy alias with its
one-line hint. It predates the 0.1 settings format (it is the command name of
the earlier ACI clients), so it is not part of the 0.1 upgrade path, and
removing it would break existing scripts for no migration benefit.
