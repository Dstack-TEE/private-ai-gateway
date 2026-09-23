# Private AI Proxy CLI

`pap` is the preferred command for managing the same per-user backend as the
desktop app; it does not require an open window. Installations keep the
canonical `private-ai-proxy` executable (including the verifier),
`private-ai-proxy-service`, and the credential helper together; see
[distribution](cli-distribution.md). `private-ai-proxy` is the full-name alias
and `aci` is the protocol-focused alias. All three names accept the same
commands and run the same implementation.

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

`private-ai-proxy` and `aci` accept these same commands. They are compiled from
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

The backend service can also serve the desktop renderer to a browser on
`127.0.0.1`. It is off by default and is not available in the Mac App Store
build. Turn it on in the desktop app's Settings or with the settings command;
changes apply immediately without restarting the service:

```sh
pap settings set webUi true
pap settings set webUiPort 4182   # default; must differ from the Local API (4180) and 4181
pap settings show                 # preferences plus the web UI address or bind error
pap settings set webUi false      # closes the listener and ends every browser session
```

If the port cannot be opened (for example, `Port 4182 is already in use on
127.0.0.1`), the service keeps running and reports the error in `pap status`,
`pap settings show` and the desktop Settings page.

Sign in with a one-time link:

```sh
pap app open --web
```

`pap app open` still opens the installed desktop app when one is present and a
graphical session is available. Without either, or with `--web`, it asks the
service over the authenticated management endpoint for a login code and prints
`http://127.0.0.1:PORT/#code=…`. It opens a browser only in a local graphical
session and never falls back to a terminal browser. When the web UI is off, it
asks `Web UI is off. Enable it on 127.0.0.1:<port>? [y/N]`; `--yes` enables it
without prompting, and `--non-interactive` without `--yes` fails with a hint to
run `pap settings set webUi true`.

The code works once and expires after 60 seconds. The page removes it from the
address bar before its first request and exchanges it for a session token that
stays in the tab's `sessionStorage`, so reloading keeps the session. Opening the
same link again, or in another tab, shows "This sign-in link has expired or was
already used"; run `pap app open --web` for a new link. Sessions end after an
hour without requests or an open page, when the web UI is turned off, and when
the service restarts.

Security model:

- The management socket or named pipe, restricted to the current user, is the
  root of trust. Only a client that can reach it can mint a login code. Codes
  and session tokens are stored only as hashes and never appear in `status`,
  `settings show`, logs or process arguments. A browser launched by `pap app
  open` receives the short-lived code in its arguments; it is useless once
  exchanged or expired.
- The listener binds only `127.0.0.1`. Requests must carry exactly
  `Host: 127.0.0.1:PORT` (blocking DNS rebinding and `localhost` resolving to
  another address) and a same-origin `Origin`. Mutations are JSON `POST`
  requests; cross-origin pages cannot add the `Authorization` header because
  no CORS preflight is ever granted.
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
| OS notifications and native updates | Hidden; update ownership stays with the installer or package manager. |
| CLI registration | Hidden. |
| RedPill loopback OAuth | Use **Paste callback link** when the browser cannot reach port 4181 on the service machine. Phala device flow is unchanged. |

For a remote machine, keep the listener on loopback and forward it over SSH:

```sh
# Remote shell
pap settings set webUi true --yes
pap app open --web

# Local shell
ssh -N -L 4182:127.0.0.1:4182 user@example-host
```

Open the printed link locally within 60 seconds. The local and remote ports
must match, because the service checks the exact `Host` header.

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
credential-store unlock probe.

## Coverage

| Core capability | CLI |
| --- | --- |
| Backend and protection lifecycle | `service`, `start`, `stop`, `status --watch` |
| Browser management UI | `settings set webUi true`, `app open --web` |
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
in the desktop UI or OS installer. Shared notification preferences are available
through `settings`; they do not grant OS notification permission. CLI-only use
still requires an accessible OS credential store. There is no plaintext fallback.
