# PAP CLI

`pap` manages the same per-user backend as the desktop app. It does not require
an open window. Installations must keep `pap` (including the verifier), `pap-service`, and the
credential helper together; see [distribution](CLI-DISTRIBUTION.md).

## Discover Commands

Start with `pap --help` and `<command> --help`. `pap schema` prints the command
tree as JSON, derived from the same Clap definitions used for parsing. It is a
discovery document, not an RPC or response JSON Schema.

`pap completions bash` prints shell completion code without installing it or
changing shell configuration. Other supported shells are listed in its help.

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
