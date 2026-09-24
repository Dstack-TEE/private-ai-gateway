# Private AI Proxy CLI

`pap` is the preferred command for managing the same per-user backend as the
desktop app; it does not require an open window. Installations keep the
canonical `private-ai-proxy` executable (including the verifier),
`private-ai-proxy-service`, and the credential helper together; see
[distribution](cli-distribution.md). `private-ai-proxy` is the canonical
executable name. `aci` is a legacy alias for existing scripts; it accepts the
same commands and runs the same implementation, and on an interactive terminal
outside `--json`/`--json-events` it prints a one-line note pointing to `pap`.
Prefer `pap` or `private-ai-proxy` in new scripts and documentation.

## Discover Commands

Start with `pap --help` and `<command> --help`. `pap schema` prints the command
tree as JSON, derived from the same Clap definitions used for parsing. It is a
discovery document, not an RPC or response JSON Schema.

`pap completions bash` prints shell completion code without installing it or
changing shell configuration. Other supported shells are listed in its help.

## ACI Commands

The same binary includes the ACI protocol commands:

| Command | Purpose |
| --- | --- |
| `pap verify <url>` | Verify the service identity and attestation. |
| `pap audit` | Audit saved ACI evidence offline; see `audit --help` for inputs. |
| `pap sessions <url>` | Inspect and verify attested inference sessions. |
| `pap send <url>` | Send an inference request using the ACI client. |
| `pap serve <url>` | Run the local streaming proxy with post-delivery receipt audits. |

`private-ai-proxy` (and the legacy `aci` alias) accept these same commands. They are compiled from
this package's ACI modules, not forwarded to another executable. `serve` is standalone;
`start` below manages the persistent background service and saved profiles.

## Lifecycle

```sh
pap service start
pap profiles list
pap start --profile work --timeout 90
pap status
pap stop
pap service stop --yes
```

`service start` starts the management backend; saved `connectOnLaunch` may also
start protection. `start` waits for verified protection. `stop` stops protection
and restores managed agent configuration but keeps management available.
`service stop` shuts down the backend. Closing the desktop app does not stop it.

## Web UI

The backend service can also serve the desktop renderer to a browser, like the
web dashboards of other proxy tools. It is off by default, listens on
`127.0.0.1` unless you allow network access, requires a sign-in password, and is
not available in the Mac App Store build. Set it up in the desktop app's
Settings > Web UI, or with the settings command; changes apply immediately
without restarting the service:

```sh
printf '%s\n' "$PASSWORD" | pap settings set webUiPassword --value-stdin --yes
pap settings set webUiPassword    # or type it twice at a hidden prompt
pap settings set webUi true
pap settings set webUiPort 4182   # default; must differ from the Local API (4180) and 4181
pap settings show                 # preferences, the web UI address or bind error, and passwordSet
pap settings set webUi false      # closes the listener and ends every browser session
```

The password must be at least 12 characters (at most 256); there are no other
composition rules. It is never accepted as a command-line argument: pass it on
stdin with `--value-stdin` (one trailing newline is dropped) or at the hidden
prompt. Changing it ends every browser session. While the web UI is off,
`pap settings set webUiPassword ""` removes it. Turning the web UI on without a
password fails with a hint to set one, and saved settings from an older version
that are on without a password leave the listener closed until one is set.

Its port may be any of 1–65535, while the Local API requires 1024 or above: the
Local API port is written into every connected agent's configuration and must
bind for protection to work, whereas a web UI bind failure is only reported in
its status. The listener otherwise uses the same rules as the Local API's
`listenAddress`, `allowNetworkAccess` and `clientHost`, under `webUi`-prefixed
keys:

| Key | Default | Meaning |
| --- | --- | --- |
| `webUiPassword` | unset | Sign-in password; required before `webUi` can be `true`. Read from stdin or a hidden prompt, stored only as an Argon2id hash. |
| `webUiListenAddress` | `127.0.0.1` | IPv4 or IPv6 address to bind. |
| `webUiAllowNetworkAccess` | `false` | Required before binding any non-loopback address. |
| `webUiClientHost` | unset | Hostname or IP in the printed address and accepted as `Host`. Required when listening on `0.0.0.0` or `::`. |

Changing the address, port or client host moves the listener at once and ends
every browser session. If the address cannot be opened (for example, `Port 4182
is already in use on 127.0.0.1`), the service keeps running and reports the
error in `pap status`, `pap settings show` and the desktop Settings page. Saved
settings that fail validation leave the web UI closed.

Open `http://HOST:PORT/` (the client host or, without one, the listen address)
and sign in with the password. The desktop Web UI settings have **Open in
Browser**, and in a terminal:

```sh
pap app open --web
```

`pap app open` still opens the installed desktop app when one is present and a
graphical session is available. Without either, or with `--web`, it opens the
web UI address in a browser in a local graphical session (never a terminal
browser) and prints it. The address carries no secret. When the web UI is off
but has a password, it asks `Web UI is off. Enable it on <address>:<port>?
[y/N]`; `--yes` enables it without prompting, and `--non-interactive` without
`--yes` fails with a hint to run `pap settings set webUi true`.

Signing in sets a session cookie, so reloading and other tabs of the same
browser share the session until it ends. Sessions end
after an hour without requests or an open page, 12 hours after sign-in even
while a page stays open, when the page signs out (Settings > Connections > Sign
Out), when the password changes or is removed, when the web UI is turned off or
its listener moves, when settings are reset, and when the service restarts. The
page then returns to the sign-in page. A signed-in browser can change the
password in Settings > Web UI after entering the current one; it stays signed
in with a fresh session cookie while every other session ends.

Security model:

- The password is stored only as an Argon2id hash (19 MiB, 2 passes, 1 lane:
  the OWASP minimum) in the owner-only `preferences.json`. Management reads of
  preferences, `status`, `settings show`, diagnostics and logs never include
  the password or its hash; they show only `passwordSet`.
- Sessions are server-side. The browser holds only a 256-bit random token in a
  `pap_session_<port>` cookie with `HttpOnly; SameSite=Strict; Path=/` and a
  12-hour `Max-Age`; the service stores only its SHA-256 digest, so page
  scripts never see it. The cookie cannot be `Secure` because the listener
  speaks plain HTTP, even on loopback. Cookies are not scoped to a port, so the
  name carries the port: a local web UI and a tunneled one on another port keep
  separate sessions. Signing out, and any response to an ended session,
  expires the cookie.
- The management socket or named pipe, restricted to the current user, is the
  root of trust: the desktop app and CLI set or remove the password over it
  without knowing the old one. A browser must already be signed in and enter
  the current password to change it.
- The listener binds `127.0.0.1` by default. A non-loopback address fails
  closed unless `webUiAllowNetworkAccess` is `true`.
- Requests must carry an allowed `Host`: the bound `IP:PORT`, the client
  `HOST:PORT`, and `127.0.0.1:PORT` (or `[::1]:PORT`) when bound to every
  interface. Anything else, including `localhost`, is refused, which blocks DNS
  rebinding.
- Cross-site request forgery (following the OWASP CSRF cheat sheet): every
  request that is not a `GET` or `HEAD` must carry an `Origin` that names that
  same host exactly, or it is refused even with a valid cookie; this is the
  primary defense, since pages on another port of the same host are the same
  site to `SameSite`. `SameSite=Strict` keeps the cookie off requests started
  by other sites as defense in depth. Reads carrying `Sec-Fetch-Site` or
  `Referer` must also name this host. Mutations are `POST` with a JSON body
  (sign-out is `DELETE`), which HTML forms cannot send; `GET` never changes
  state; no CORS access is ever granted, so cross-origin pages cannot read
  responses; and the CSP sets `form-action 'none'` and `frame-ancestors 'none'`.
- The page and its assets load without a session; every `/api` route except
  sign-in requires one. Sign-in attempts, password changes from a browser and
  rejected API requests draw from a per-client rate limit: a burst of 10, then
  one every 3 seconds, answered with `429` and `Retry-After`. A wrong password
  and an unset one get the same answer. Each IPv4 address and IPv6 /64 has its
  own budget, so one client cannot delay sign-ins from others. Clients reaching
  a loopback listener through a TCP forwarder all appear as that forwarder and
  share its budget. Use a long, unique password on any network listener.
- Browser requests run through the same command admission and dispatch as the
  management endpoint, and errors carry the same sanitized messages as the
  desktop app. Responses set a restrictive CSP, `nosniff`, `no-store` and
  `no-referrer`.
- A session has the same authority as the desktop app, including reading the
  Local API client key. Treat an open session like an unlocked desktop app.

The browser UI degrades desktop-only integration:

| Feature | Web behavior |
| --- | --- |
| Profile import and exports | Browser file picker and downloads; browser paths are never sent to the service. |
| Native child windows and dialogs | In-page modal sheets and dialogs. |
| Clipboard and external links | Browser clipboard and allowlisted HTTPS tabs. |
| Open at Login, tray/menu state | Hidden. Protect on launch remains shared with the backend. |
| OS notifications | Hidden. |
| Software updates | Settings > About announces a newer release in the saved channel with the exact upgrade steps for the backend's installation; it never installs anything. |
| CLI registration | Hidden. |
| RedPill loopback OAuth | Use **Paste callback link** when the browser cannot reach port 4181 on the service machine. Phala device flow is unchanged. |

### Remote Access

Prefer these options, in order. Each needs a password first
(`pap settings set webUiPassword`).

1. **SSH tunnel.** Keep the listener on loopback and forward it:

   ```sh
   # Remote shell
   pap settings set webUi true --yes

   # Local shell
   ssh -N -L 4182:127.0.0.1:4182 user@example-host
   ```

   Then open `http://127.0.0.1:4182/` locally. The local and remote ports must
   match, because the service checks the exact `Host` header.

2. **Tailscale.** Keep the listener on loopback and forward the port to your
   tailnet with a raw TCP forwarder. WireGuard encrypts the traffic, and only
   devices your tailnet policy allows can connect. Set the client host to the
   machine's MagicDNS name so the printed address and the `Host` check match:

   ```sh
   tailscale serve --bg --tcp 4182 tcp://127.0.0.1:4182
   pap settings set webUiClientHost example-host.tailnet-name.ts.net --yes
   pap settings set webUi true --yes
   pap app open --web    # http://example-host.tailnet-name.ts.net:4182
   ```

   Use `--tcp`, not the default HTTPS proxy: that proxy presents an `https://`
   origin on port 443, which the service rejects. Remove the forwarder with
   `tailscale serve --tcp=4182 off`.

3. **Direct LAN listening.** Bind a LAN address only on a network you trust:

   ```sh
   pap settings set webUiAllowNetworkAccess true --yes
   pap settings set webUiListenAddress 192.168.1.20 --yes
   # or every interface, with the name clients use:
   # pap settings set webUiClientHost studio.local --yes
   # pap settings set webUiListenAddress 0.0.0.0 --yes
   pap app open --web    # http://192.168.1.20:4182
   ```

   The connection is **unencrypted HTTP**. Anyone who can observe or
   intercept traffic on that network can read the password as it is sent, the
   session cookie and every page, including the Local API client key, and can
   act as the signed-in user. Use this only on a trusted network with a
   password you use nowhere else, and never expose the port to the internet,
   through port forwarding or otherwise. The desktop Settings page shows the
   same **Non-loopback** warning and asks for confirmation before saving.

Browsers treat plain HTTP on any address other than `127.0.0.1` as insecure, so
over Tailscale or a LAN the copy buttons are unavailable; select and copy text
instead.

The user session survives transport failures, retries and profile changes until
protection is explicitly stopped. After an abnormal backend exit, its session ID
and usage can be resumed; verification and forwarding permission are never
restored from disk.

### Reset Settings

`pap settings reset --yes` stops protection, disconnects managed agents, and
restores backend preferences, the default Local API listener, and the production
OS policy. Profiles, credentials, the local client key and usage history are kept.
The same operation is available under Settings > Advanced in the desktop, which
also disables Open at Login. CLI installation and system notification permission
are unchanged. Failures are reported; retry after resolving the reported conflict.

Human `status` summarizes the backend PID/version, active profile and service,
saved credential presence (not unlock status), Local API exposure, production OS
policy, TEE identity/checks, catalog size and current-session usage. Retained
catalogs are labeled cached when protection is inactive. Reported costs are
session totals, not a billing reconciliation. Request contents and tokens are
never included in this summary. `--json` retains the full existing state shape.

Read-only commands do not start a missing backend. A successful `status` means
the query succeeded, not that protection is active: inspect `gateway.status`
and `gateway.configurationVerification` in JSON. Connection and configuration
states do not by themselves establish a verified inference session.

## Profiles And Credentials

```sh
pap profiles show work
pap profiles edit work --name "Work gateway"
pap profiles edit work --require-production-os
pap profiles verify work
pap profiles export --output profiles.json
pap profiles import profiles.json --yes
```

Adding, editing and verifying a profile use the backend's verification and
credential-storage policy. Verification can change the active profile and
restart protection; it is not a read-only probe. Credentials use a hidden prompt
or explicit `--key-stdin`. Never pass a credential as an argument. Changing a
service URL requires a credential for that destination, not implicit forwarding
of the old one. Exported profiles do not contain credentials.

`--key-stdin` is for a pipe or redirected file, not a terminal. The production
OS requirement is a shared current policy, not a separate preference per profile;
`edit --require-production-os` restores the strict policy.

## Automation

Use `--json --non-interactive` for data and `--yes` only for changes you intend
to approve. These flags are independent: JSON and non-interactive mode never
imply consent. Results go to stdout; failures go to stderr. Watch mode emits
one JSON snapshot per line. Human-readable output is not a parsing contract.

For reviewed agent changes, obtain a preview first:

```sh
pap --json agents connect codex --model MODEL --dry-run
pap --json --yes agents connect codex --model MODEL --revision REVISION
```

Use the returned revision with the same agent, direction and options. A changed
configuration is rejected rather than silently re-previewed and approved.
Do not automatically retry mutations after a timeout or lost connection: they
may have completed. Query state before deciding what to do next.

Exit status is `0` for successful execution, `2` for invalid command arguments,
and `1` for operation failures. With `--json`, failures have an `error` object
with `code` and `message`. Argument errors use `invalid_arguments`; operation
errors currently use `command_failed`. Do not classify failures by parsing
human message text or assume all failures are retryable.

`doctor` reports every independent check, even when some fail. In that case it
prints the partial report on stdout and exits nonzero; the `errors` object
identifies failed checks. `credentialPolicy` describes policy, not an actual
credential-store unlock probe. The `update` check is advisory: when the release
feed is unreachable it reports an `error` inside `update` without failing
`doctor`.

## Coverage

| Core capability | CLI |
| --- | --- |
| Backend and protection lifecycle | `service`, `start`, `stop`, `status --watch` |
| Browser management UI | `settings set webUiPassword --value-stdin`, `settings set webUi true`, `webUiListenAddress`/`webUiAllowNetworkAccess`/`webUiClientHost`, `app open --web` |
| Profile inspection, verification and selection | `profiles list/show/add/edit/verify/use/remove` |
| Credential replacement and removal | `profiles verify --key-stdin`, `token clear-credential` |
| Agent configuration review and restoration | `agents list/connect/disconnect/disconnect-all` |
| Verified model catalog | `models list --refresh` |
| Usage, filtering, pagination, CSV and deletion | `usage list/show/export/clear` |
| Shared preferences and Local API settings | `settings show/set` |
| Local inference token | `token show/rotate` |
| Configuration backups and redacted diagnostics | `profiles import/export`, `diagnostics` |
| CLI registration and app opening | `cli status/install/uninstall`, `app open` |
| Installation and connection diagnostics | `doctor` |

Usage time filters are Unix seconds. List pagination uses `--cursor` and
`--limit`; CSV export covers all records matching its filters, not just the
currently displayed page. Exports refuse existing destination files.

OS login startup, notification permissions and installer-based app updates stay
in the desktop UI or OS installer. `doctor` also reports whether the saved update
channel (`settings set updateChannel beta|stable`) has a newer release and the
exact upgrade steps for this installation; see
[Updates by installation](distribution.md#updates-by-installation). Shared notification preferences are available
through `settings`; they do not grant OS notification permission. CLI-only use
still requires an accessible OS credential store. There is no plaintext fallback.
