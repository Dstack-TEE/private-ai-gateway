# Settings files

Private AI Proxy keeps every user setting in two TOML files in one settings
directory, which holds nothing else:

| File | Holds | Permissions |
| --- | --- | --- |
| `config.toml` | Profiles (without keys), the active profile, the Local API and web UI listeners, appearance, notifications, update channel, connect on launch, command registration | Owner-only when created; never holds a secret |
| `credentials.toml` | The user's credentials: provider API keys (manual and account sign-in) and the web UI password hash | Always owner-only (`0600`) on macOS and Linux |
| `config.schema.json` | The JSON Schema of `config.toml`, rewritten by each version | Generated |

The layout follows established tools: one `config.toml` like Cargo
(`~/.cargo/config.toml`) and Codex (`~/.codex/config.toml`), with named
`[profiles.<id>]` tables selected by `activeProfile` as in Codex; secrets in a
separate owner-only `credentials.toml` like Cargo's `credentials.toml` and the
AWS CLI's `credentials`. Keys use the same camelCase names as the management
API and `pap settings set`, as `Tauri.toml` does.

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
whole. Linux follows the [XDG base directories](https://specifications.freedesktop.org/basedir/latest/).
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

The app writes regular files atomically and refuses to replace a symlink; to
share settings, sync the directory rather than linking the files.

## Syncing between devices

The settings directory contains only settings, so it can be synced as a whole
(a dotfiles repository, Syncthing, a cloud folder) on every platform.

- `config.toml` is always safe to sync. Listener addresses and ports apply to
  every synced device.
- `credentials.toml` holds only the user's credentials (API keys, the web UI
  password hash), so it is safe to sync with `config.toml`, but it is
  plaintext: sync it only through storage you trust with those secrets.
  Otherwise leave it out and enter keys on each device.
- `config.schema.json` is generated; each version rewrites it on start.
- Never sync the app data directory: it is this device's state, including
  `local-state.json`.

## Security

`credentials.toml` is plaintext protected by file permissions, like Cargo's and
the AWS CLI's credential files: owner-only (`0600`) in an owner-only (`0700`)
directory on macOS and Linux, and the per-user profile ACL on Windows, like
every other private file of the app. Every write restores `0600`; `pap doctor`
warns when the file is readable by others. `local-state.json` in the app data
directory gets the same treatment. Neither file is ever shown by the app:
`settings show`, `status`, diagnostics and logs omit their contents.
Anyone who can read your files as you can read it, which is also true of an
unlocked OS keychain for a process running as you.

## Upgrading from 0.1

0.1 stored settings in `confidential-ai.json`, `local-api.json` and
`preferences.json` in the app data directory, and API keys, account keys and
agent restore values in the OS credential store (macOS Keychain, Windows
Credential Manager, Secret Service). On the first start of 0.2 the backend
imports them once:

1. It reads the old files and every credential store entry they reference
   (nothing else in the credential store is touched), writes
   `credentials.toml` (user credentials) and `local-state.json` (device-local
   secrets), syncs them to disk and reads them back.
2. Only then does it delete the imported credential store entries.
3. It writes `config.toml`, which marks the import as done.
4. It moves the old files to `migrated-0.1/` in the app data directory as a
   one-time backup. They contain no API keys; `preferences.json` contains the
   web UI password hash. Delete the backup once you are satisfied.

An interrupted import reruns on the next start and keeps whatever it already
moved. If the credential store is locked or unavailable (for example Linux
without a Secret Service, or a denied macOS prompt), startup continues: the
settings are imported, Settings and `pap status` show which profiles need their
API key re-entered, and `pap doctor` lists profiles without a saved key. The
credential store is asked at most once.

The credential store reader exists only for this import and will be removed in
a later release (planned for 0.3), together with the `keyring` dependency.

### Tracking: Mac App Store keychain entitlement

The Mac App Store build keeps its Keychain access group entitlement only so the
backend can import and delete what 0.1 saved in the Keychain. Remove it from
the App Store entitlements (see [Mac App Store](mac-app-store.md)) in the same
release that removes `runtime/src/settings/legacy.rs` and `keyring` (planned
for 0.3).
