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
[Helix](https://docs.helix-editor.com/configuration.html). `pap settings set`
names a key by its dotted path in the file (`web-ui.port`), as `git config`
and `cargo config get` do, and `pap settings show` prints the same names.

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

The service generates the web UI password when none is set, as code-server
does on first run; `pap web-ui password show` prints it and
`pap web-ui password rotate` replaces it (see [the CLI guide](cli.md#web-ui)).
Earlier versions kept only `password-hash`, an Argon2id hash of a chosen
password; it keeps signing in until a new password replaces it.

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
| macOS (direct download) | `~/Library/Application Support/org.dstack.private-ai-proxy/Config` | `~/Library/Application Support/org.dstack.private-ai-proxy` |
| macOS (Mac App Store) | `~/Library/Containers/org.dstack.private-ai-proxy/Data/Library/Application Support/org.dstack.private-ai-proxy/Config` | The same path without `Config` |
| Windows | `%APPDATA%\org.dstack.private-ai-proxy\Config` | `%APPDATA%\org.dstack.private-ai-proxy` |

The settings directory is always separate from state, so it can be synced as a
whole. Linux follows the [XDG base directories](https://specifications.freedesktop.org/basedir/latest/);
as the specification requires, a relative `XDG_CONFIG_HOME` or `XDG_DATA_HOME`
is ignored.
macOS and Windows keep one directory per app (Tauri's `app_data_dir`); settings
get their own `Config` subdirectory in it, as VS Code keeps its settings in
`~/Library/Application Support/Code/User` and `%APPDATA%\Code\User`, the
directory its Settings Sync syncs. `pap settings show` prints the paths in use.

Overrides follow the existing `PRIVATE_AI_PROXY_*` variables:

- `PRIVATE_AI_PROXY_CONFIG_DIR`: exact settings directory (absolute path).
- `PRIVATE_AI_PROXY_DATA_DIR`: exact app data directory; without a settings
  override the settings live in its `Config` subdirectory (the Mac App Store
  build sets it to its container).
- `PRIVATE_AI_PROXY_HOME`: test home; data in `<home>/.private-ai-proxy`,
  settings in `<home>/.private-ai-proxy/Config`.

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
   time to answer a macOS Keychain prompt. Until the step is recorded, even
   across restarts after a timeout or a denied prompt, disconnecting an agent
   whose original key has not been imported yet fails with the same "agent
   credential could not be restored" error 0.1 gave while the credential
   store was unavailable, and succeeds once the key is imported, instead of
   dropping that key.

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

A step that fails writes nothing that records it: the old files and the
credential store entries stay where they are, the error stays in Settings,
`pap status` and `pap doctor`, and the step runs again on the next start. While
step 1 has not succeeded, settings cannot be changed, so nothing can be saved
that the import would then have to merge with. If the credential store is
locked, unavailable (for example Linux without a Secret Service) or a macOS
prompt is denied or left unanswered, the settings are still in effect,
Settings and `pap doctor` name the profiles whose API key to re-enter, and the
next start tries the store again. Keys you entered in the meantime are kept.

The backup contains no API keys; `preferences.json` contains the web UI
password hash. Delete `migrated-0.1/` once you are satisfied (keep
`import-complete`, or the next start looks for credentials to import once
more, which is harmless).

## Removal in 0.3

The single list of compatibility code to remove in 0.3, once upgrading from
0.1 directly to 0.3 is no longer supported. Each item exists only for
installations of 0.1 or for command-line options deprecated in 0.2; other
documents link here instead of keeping their own lists.

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
