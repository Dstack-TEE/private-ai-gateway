# Private AI Proxy CLI

`pap` is the command-line interface of Private AI Proxy. It manages the same
per-user backend service as the desktop app, without an open window, and
includes the ACI verification commands. Every installation keeps the
`private-ai-proxy` executable, `private-ai-proxy-service` and the credential
helper together; see [CLI distribution](cli-distribution.md).

`pap` is the preferred command, and `private-ai-proxy` is the canonical
executable name. `aci` is a legacy alias kept for existing scripts. All three
run the same executable with the same commands. On an interactive terminal
outside `--json` and `--json-events`, `aci` prints a one-line note pointing to
`pap`. Use `pap` or `private-ai-proxy` in new scripts and documentation.

## Discover Commands

Start with `pap --help` and `<command> --help`.

`pap completions bash` prints shell completion code without installing it or
changing shell configuration. Other supported shells are listed in its help.

## ACI Commands

These commands are the relying party of the
[ACI protocol](../../../spec/aci.md):

| Command | Purpose |
| --- | --- |
| `pap verify <url>` | Fetch the attestation report with a fresh nonce, run the spec 9.1 identity checks, and print the transcript. Exits 0 only when the verdict is `VERIFIED`. |
| `pap audit` | Run the same checks offline over saved artifacts (report, receipt, bodies, session); see `audit --help` for inputs. |
| `pap sessions <url>` | Audit the service's current attested sessions (spec 9.2), optionally under a `--require-claim` policy. The ids that pass are what you pin (spec 5.3). |
| `pap send <url>` | Send one chat completion over the pinned channel, then verify its receipt and the session it cites. |
| `pap curl <https-url> -- [options]` | Verify the service, then run system curl for one request with the attested TLS key pinned. |
| `pap serve <url>` | Run a [local verifying proxy](#local-verifying-proxy) that streams over the pinned channel and audits receipts after delivery. |

`verify`, `audit`, `sessions`, `send` and `curl` do not start the backend
service or read the settings files.

The pinned channel uses the TLS keys the verified report declares for the
host: every attested TLS key when none is domain-scoped, otherwise only the
entry its `downstream_tls_binding` names (spec 4.2). A failed verification or a
pin mismatch stops the request.

`send` reads the API key from the `ACI_API_KEY` environment variable or, with
`--api-key-stdin`, from stdin (as `docker login --password-stdin` does), never
from an argument that other local processes can see. The old `--api-key KEY`
option is hidden, still works with a warning, and will be removed in 0.3
(see [Removal in 0.3](configuration.md#removal-in-03)).

### Verification Policy

`verify`, `audit`, `sessions`, `send`, `curl` and `serve` accept the same
policy flags:

| Flag | Meaning |
| --- | --- |
| `--accept-compose <hex>` | Compose hash to accept; repeatable. Without it the compose measurement is verified and reported, and you appraise its provenance yourself. |
| `--accept-subject app-id:0x<hex>` | Measured dstack app-id to accept for key custody; repeatable. |
| `--accept-dstack-kms-root-public-key <hex>` | dstack KMS root public key the key-custody chain must end at; repeatable. |
| `--require-production-os` | Require the attested OS image to be a reviewed production image. |

Key custody (check id-5) is checked when both custody flags are given; one
without the other is an error. The receipt key's dstack KMS signature chain
must then end at an accepted root, anchored on the app-id that the verified
event log measures. The report's self-asserted `image_digest` is never an
anchor: nothing measured corroborates it (spec 4.1), so any app under the same
KMS root could claim it. Without the custody flags id-5 is an honest skip; no
trust anchors are built in.

Under `--require-production-os`, the client reads the RTMR3-bound
`os-image-hash` and requires it to be in the verifier's reviewed
production-image allowlist. Development and unknown hashes fail closed.
Updating the allowlist requires a verifier release.

This option is an appraisal step, not a dstack boot verifier. The client
verifies the DCAP quote and replays RTMR3, but it does not reconstruct MRTD or
RTMR0-2 from the dstack OS image. Before relying on `policy-os: pass`, run a
dstack verifier over the same quote, event log, and VM configuration, and
require `is_valid: true`; that result establishes `os_image_hash` from those
boot measurements. See
[How the OS image is classified](../../../docs/providers/phala-direct/verification.md#how-the-os-image-is-classified).

### One pinned curl request

```sh
pap curl https://tee.redpill.ai/v1/chat/completions -- \
  --fail-with-body --no-buffer \
  --header "Authorization: Bearer $ACI_API_KEY" \
  --header 'content-type: application/json' \
  --data-binary '{"model":"MODEL_ID","messages":[{"role":"user","content":"Hi"}],"provider":{"aci_verified":true}}'
```

`pap curl` verifies a fresh service report under the
[verification policy](#verification-policy) flags, then starts system curl with
the attested TLS key pinned. The response stays on stdout and verification
output goes to stderr; with `--json` the transcript is one JSON line on stderr.
This command does not audit the response receipt; use `pap send` or
`pap serve` when that is required.

curl also validates the certificate chain against the system CA store, so the
service needs a CA-issued certificate. Production deployments normally have one.

The wrapper supports a single URL and these curl request options:

| Options | Purpose |
| --- | --- |
| `--header`, `-H`, `--data`, `-d`, `--data-raw`, `--data-binary`, `--json`, `--form`, `-F`, `--upload-file`, `-T`, `--request`, `-X` | Build the request. |
| `--fail`, `-f`, `--fail-with-body`, `--no-buffer`, `--silent`, `-s`, `--show-error`, `-S`, `--include`, `-i`, `--verbose`, `-v`, `--compressed`, `--head`, `-I` | Control output and transfer behavior. |
| `--output`, `-o`, `--max-time`, `--connect-timeout` | Write the result or set timeouts. |

Put options after `--` and give each value-taking option a separate argument.
Additional URLs, redirects, proxy or TLS overrides, config files, and other
curl options are rejected; this is deliberately not a general curl parser. The
URL is never globbed, so `[1-3]` or `{a,b}` in it is sent as written.

| Exit code | Meaning |
| --- | --- |
| 125 | pap refused the request or could not start curl: invalid options, failed verification, or curl missing. curl did not run. |
| 128 + N | curl was killed by signal N (Unix). |
| Any other | curl's own exit code; 0 is success. |

Command-line usage errors, such as a missing URL, exit 2 before verification.

### Local verifying proxy

`pap serve <url>` verifies the service, prints the transcript, and refuses to
start unless the verdict is `VERIFIED`. It then listens on plain HTTP at
`127.0.0.1:4180` (`--listen` changes the address), so any OpenAI- or
Anthropic-compatible client can use it as a local base URL.
[Quickstart step 4](../../../docs/quickstart.md#4-use-it-as-a-local-endpoint)
walks through it against a live service.

Requests:

- Every method and path is forwarded to the same path on the service over the
  pinned channel, with headers passed through in both directions except
  hop-by-hop headers. Bodies are never logged or written to disk.
- A POST that carries E2EE request headers (E2EE v2 or the legacy transport)
  is rejected with HTTP 400 instead of forwarded. Send plaintext bodies.
- Every JSON POST body gets `provider.aci_verified: true` (spec 5.3), so an
  aggregator refuses rather than serve the request through an unverified
  upstream. `--allow-unverified` drops this demand.
- Session pinning is opt-in, in one of two ways that cannot be combined.
  - `--session <id>` (repeatable) defines a fixed accepted set. Each POST uses
    the intersection with its own pins, or this set when it has none, and a
    request whose pins are disjoint from it is rejected locally. The set is
    never refreshed: when the service refuses a superseded pin, its HTTP 412
    reaches the client unchanged.
  - `--require-claim <name[=source]>` derives the pin set from the audited
    current sessions, and `serve` refuses to start when no session satisfies
    the policy. When the service refuses a superseded pin (HTTP 412), the
    proxy re-derives the set and retries the request once.

  Both imply verified serving and cannot be combined with
  `--allow-unverified`.

Responses:

- Responses stream through byte-exact while the proxy digests the wire bytes.
  Receipt checks never delay delivery or retract delivered bytes.
- For each POST response with an `X-Receipt-Id`, the proxy records the receipt
  id and body digests, keeping the last 256 exchanges. After delivery it
  fetches the receipt and the session it cites, using the request's bearer
  token only for that fetch, and runs the spec 9.3 and 9.2 checks. The audit
  also requires the cited session to be one of the request's pins (9.3(6)) and
  to satisfy `--require-claim` (9.2(3)).
- A 2xx POST response without a receipt header is flagged as failed at once
  (spec 5.2), since it can never be audited.

A keyset rotation blocks forwarding until a fresh verification passes. A
changed `X-ACI-Keyset-Digest` on a response triggers it, and so does a
handshake the pin refuses, since a rotated TLS key aborts the connection
before any response exists. A failed re-verification keeps the old pin. After a
successful one, the refused request is sent once more if the identity it was
admitted under still holds, and otherwise gets a retryable 503.

Standalone `serve` also opens a control listener on `127.0.0.1:4183`
(`--control` changes it; the web UI uses 4182), backed by the same receipt
store. `GET /receipts` lists recent exchanges, newest first.
`POST /receipts/<id>/verify` runs that receipt's audit again and returns the
verdict as JSON; send the service's `Authorization` header if the receipt fetch
needs it. `pap start` runs the same verifier inside the backend service, which
opens neither listener: agents reach it through the Local API, and Usage keeps
its receipt audits.

`--json-events` (or the global `--json`) emits JSON Lines on stdout: `ready`
after verification and once the listeners are bound, with `proxy_url`,
`control_url` and the active `policy`; `request_complete` for each request and
its optional `receipt_id`; `blocked` when forwarding fails closed;
`identity_updated` after a successful re-verification; and `fatal` before an
unsuccessful exit. Human diagnostics stay on stderr.

## Lifecycle

```sh
pap service start
pap profiles list
pap start --profile work --timeout 90
pap status
pap stop
pap service stop --yes
```

`service start` starts the management backend; Protect on launch
(`connect-on-launch`) may also start protection. `start` waits for verified
protection. `stop` stops protection and restores managed agent configuration
but keeps management available.
`service stop` shuts down the backend. Closing the desktop app does not stop it.
When no backend is running, `service stop` restores nothing, and agents may
still point at the Local API: after an update that was not relaunched, a crash,
or a Windows sign-out. `stop --offline` does what `stop` does, without a
backend: it ends the saved protection session, so the next backend start does
not resume it, and restores the agents through the same restoration journal
under the same lock, keeping each connection until protection is started again.
Running it again changes nothing. It holds the backend's instance lock while it
works and is refused while a backend runs; use `stop` or `service stop` then.
The Windows uninstaller runs it after `service stop`; if it fails, the
uninstaller shows why and asks whether to uninstall anyway, keeping the restore
data (a silent uninstall continues and logs why). If the app data was also
to be deleted, it cancels instead, since the app data holds the restore data. On macOS (moving the app to the Trash) and Linux (the
packages run no removal scripts) nothing does, so run `pap --yes service stop`
and then `pap stop --offline` before removing the app. Mac App Store builds
restore agents only through the app's Stop All and Quit.
Stopping first restores the coding-agent configuration; if that fails, `service
stop` is refused so agents are not left pointing at a stopped Local API: it
reports that the backend keeps running, and the service log has the reason. Mac
App Store builds stop anyway. A signal (SIGTERM or SIGINT, as systemd, launchd
and `kill` send) always stops the backend, since a service manager would kill it
next; a restore that failed is logged and retried when the backend next starts.
A signal during a `service stop` waits for it, and stops the backend itself only
if that stop was refused. On Windows, signing out or shutting down ends the
windowless backend without this sequence; the next start resumes the
protection session it left, and until then agents stay pointed at the Local API.
Shutdown is bounded: running commands get 10 seconds, then open connections and
leftover background tasks 5 seconds each, and the process exits at the latest 30
seconds after the shutdown began.

Clients reach the backend through a socket in a per-user directory that does not
depend on how the session was started: `XDG_RUNTIME_DIR`, else `/run/user/$UID`,
on Linux, and the user's `/var/folders/…/T/` on macOS. A 0.2 beta backend
started with a custom `TMPDIR`, or on Linux without `XDG_RUNTIME_DIR`, listens
elsewhere: newer clients do not find it, and a new backend cannot start while it
runs. Stop it with the old build's `pap service stop`, or sign out or restart.

On Windows the backend starts detached from the console, like Node's `detached`
processes, but inside the caller's job object: a job that ends its processes
when it closes, as some remote shells and CI runners use, ends the backend too.

The user session survives transport failures, retries and profile changes until
protection is explicitly stopped. After an abnormal backend exit, its session ID
and usage can be resumed; verification and forwarding permission are never
restored from disk.

## Status

Human `status` summarizes the backend PID/version, active profile and service,
saved credential presence, a settings file error, Local API exposure, production OS
policy, TEE identity/checks, catalog size and current-session usage. Retained
catalogs are labeled cached when protection is inactive. Reported costs are
session totals, not a billing reconciliation. Request contents and tokens are
never included in this summary. `--json` retains the full existing state shape.

Every command that prints a state (`status`, `status --watch`, `start`, `stop`,
profile, settings and web UI changes, and account sign-in) prints it as the
management API sends it: the state's fields plus `protection`, the
presentation derived from them (`phase`, such as `protected` or
`profileRequired`; `title`; `tone`; and `action`, the operation the
protection switch offers). `protection` is additive and always describes the
state it accompanies.

Read-only commands do not start a missing backend. A successful `status` means
the query succeeded, not that protection is active: inspect `gateway.status`
and `gateway.configurationVerification` in JSON. Connection and configuration
states do not by themselves establish a verified inference session.

## Settings

Settings live in `config.toml` and the user's credentials in `credentials.toml`.
[Settings files](configuration.md) covers their locations, contents, hand
edits, syncing and the upgrade from 0.1.

```sh
pap settings show                          # settings in effect and both file paths; never secrets
pap settings set appearance dark           # edits config.toml in place, keeping comments
pap settings set local-api.port 4190       # a key is its dotted path in config.toml
pap settings schema > config.schema.json
```

A `settings set` key is the setting's dotted path in `config.toml`, the way
`git config` and `cargo config get` name keys: `appearance`,
`connect-on-launch`, `update-channel`, `auto-cli-registration`,
`notifications.enabled` (and `.gateway`, `.local-api`, `.verification`),
`local-api.listen-address`, `local-api.allow-network-access`, `local-api.port`,
`local-api.client-host`, the same four under `web-ui.`, `web-ui.enabled` and
`web-ui.password`. `settings show` prints the settings under the same names.
The flat camelCase names of 0.1 (`connectOnLaunch`, `port`, `webUi`,
`webUiPort` and so on, and `notifications` with a JSON object) still work,
print a deprecation warning naming the new key, and are removed in 0.3.

`settings set` changes go through the backend like the desktop app's Settings
page. Hand edits apply as soon as they are saved, and `settings show` reports
invalid edits and unknown keys; see [Editing](configuration.md#editing).

### Reset Settings

`pap settings reset --yes` stops protection, disconnects managed agents, and
returns every setting in `config.toml` except the profiles and the active
profile to its default (including the Local API listener and the production OS
policy), and replaces the web UI password with a new generated one. Profiles,
their API keys, the local client key and usage history are kept.
The same operation is available under Settings > Advanced in the desktop app, which
also disables Open at Login. CLI installation and system notification permission
are unchanged. Failures are reported; retry after resolving the reported conflict.

## Web UI

The backend service can also serve the desktop app's interface to a browser, like the
web dashboards of other proxy tools. It is off by default, listens on
`127.0.0.1` unless you allow network access, and is not available in the Mac
App Store build. Set it up in the desktop app's Settings > Web UI, or with the
settings command; changes apply immediately without restarting the service:

```sh
pap settings set web-ui.enabled true
pap web-ui password show              # the sign-in password; treat the output as a secret
pap app open --web                    # opens or prints the address
pap settings set web-ui.port 4182     # default; must differ from the Local API (4180)
pap settings show                     # settings, file paths, and the web UI address or bind error
pap settings set web-ui.enabled false # closes the listener and ends every browser session
```

Browsers sign in with a password. As code-server does on first run, the service
generates one (128 random bits from the operating system, as 32 hex digits)
when none is set and keeps it in the owner-only `credentials.toml`, next to the
provider API keys, so the desktop app and CLI can show it like the Local API
key. The desktop app's Settings > Web UI shows it with Copy and **Generate New
Password**; in a terminal:

```sh
pap web-ui password show              # asks first; --yes for scripts
pap web-ui password rotate            # generates a new one and signs out every browser
printf '%s\n' "$PASSWORD" | pap settings set web-ui.password --value-stdin --yes
pap settings set web-ui.password      # or type your own at a hidden prompt
```

A password you choose must be at least 12 characters (at most 256); there are
no other composition rules. Do not reuse a personal password: it is stored in
plain text so the app can show it. It is never accepted as a command-line argument:
pass it on stdin with `--value-stdin` (one trailing newline is dropped) or at
the hidden prompt. Changing or rotating the password ends every browser
session. Earlier versions kept only an Argon2id hash of a chosen password; it
keeps signing in, but cannot be shown until you rotate it or set a new one.

Its port may be any of 1–65535, while the Local API requires 1024 or above: the
Local API port is written into every connected agent's configuration and must
bind for protection to work, whereas a web UI bind failure is only reported in
its status. The listener otherwise uses the same rules as the Local API's
`local-api.listen-address`, `local-api.allow-network-access` and
`local-api.client-host`, under `web-ui.` keys:

| Key | Default | Meaning |
| --- | --- | --- |
| `web-ui.password` | generated | Sign-in password you choose instead of the generated one. Read from stdin or a hidden prompt and kept in `credentials.toml`. |
| `web-ui.listen-address` | `127.0.0.1` | IPv4 or IPv6 address to bind. |
| `web-ui.allow-network-access` | `false` | Required before binding any non-loopback address. |
| `web-ui.client-host` | unset | Hostname or IP in the printed address and accepted as `Host`. Required when listening on `0.0.0.0` or `::`. |

Changing the address, port or client host moves the listener at once and ends
every browser session. If the address cannot be opened (for example, `Port 4182
is already in use on 127.0.0.1`), the service keeps running and reports the
error in `pap status`, `pap settings show` and the desktop app's Settings page.

Open `http://HOST:PORT/` (the client host or, without one, the listen address)
and sign in with the password. The desktop app's Web UI settings have **Open in
Browser**, and in a terminal:

```sh
pap app open --web
pap web-ui password show
```

`pap app open` still opens the installed desktop app when one is present and a
graphical session is available. Without either, or with `--web`, it opens the
web UI address in a browser in a local graphical session (never a terminal
browser) and prints it. The address carries no secret. When the web UI is off,
it asks `Web UI is off. Enable it on <address>:<port>? [y/N]`; `--yes` enables it without prompting, and `--non-interactive` without
`--yes` fails with a hint to run `pap settings set web-ui.enabled true`.

Signing in sets a session cookie, so reloading and other tabs of the same
browser share the session until it ends. Sessions end
after an hour without requests or an open page, 12 hours after sign-in even
while a page stays open, when the page signs out (Settings > Connections > Sign
Out), when the password changes, when the web UI is turned off or its listener
moves, when settings are reset, and when the service restarts. The page then
returns to the sign-in page. Browsers cannot read or change the password; as
with code-server and Jupyter, it is managed outside the browser.

Security model:

- The password is kept in the owner-only `credentials.toml`, like the provider
  API keys, because the desktop app and CLI show it; a hash beside it would
  protect nothing from someone who can read that file. Settings reads,
  `status`, `settings show`, diagnostics and logs never include it. Sign-in
  compares SHA-256 digests in constant time. A hash kept by an earlier
  version is verified with Argon2id until replaced.
- Sessions are server-side. The browser holds only a 256-bit random token in a
  `pap_session_<port>` cookie with `HttpOnly; SameSite=Strict; Path=/` and a
  12-hour `Max-Age`; the service stores only its SHA-256 digest, so page
  scripts never see it. The cookie cannot be `Secure` because the listener
  speaks plain HTTP, even on loopback. Cookies are not scoped to a port, so the
  name carries the port: a local web UI and a tunneled one on another port keep
  separate sessions. Signing out, and any response to an ended session,
  expires the cookie.
- The management socket or named pipe, restricted to the current user, is the
  root of trust: only the desktop app and CLI read, rotate or set the password
  over it. The web UI answers those methods as unknown.
- The listener binds `127.0.0.1` by default. A non-loopback address fails
  closed unless `web-ui.allow-network-access` is `true`.
- Requests must carry an allowed `Host`: the bound `IP:PORT`, the client
  `HOST:PORT`, `127.0.0.1:PORT` (or `[::1]:PORT`) when bound to every
  interface, and `localhost:PORT` when loopback reaches the listener. Browsers
  resolve `localhost` only to loopback (RFC 6761), so, as in Syncthing's GUI, no
  page can rebind it. Any other name is refused, which blocks DNS rebinding.
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
  sign-in requires one. Sign-in attempts and rejected API requests draw from a
  per-client rate limit: a burst of 10, then
  one every 3 seconds, answered with `429` and `Retry-After`. A wrong password
  and an unset one get the same answer. Each IPv4 address and IPv6 /64 has its
  own budget, so one client cannot delay sign-ins from others. Clients reaching
  a loopback listener through a TCP forwarder all appear as that forwarder and
  share its budget. At most two password checks run at once across all
  clients, which bounds Argon2 memory however many addresses send requests.
  A password you choose for a network listener should be long and unique.
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
| Clipboard and external links | Browser clipboard and allowlisted HTTPS tabs. |
| Open at Login, tray/menu state | Hidden. Protect on launch remains shared with the backend. |
| OS notifications | Hidden. |
| Software updates | Settings > About announces a newer release in the saved channel with the exact upgrade steps for the backend's installation; it never installs anything. |
| CLI registration | Hidden. |
| Account sign-in | RedPill and Phala both use a device code, so the sign-in can be approved from any browser; no callback reaches the service machine. |

### Remote Access

Prefer these options, in order. Sign in with the password from
`pap web-ui password show`.

1. **SSH tunnel.** Keep the listener on loopback and forward it:

   ```sh
   # Remote shell
   pap settings set web-ui.enabled true --yes

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
   pap settings set web-ui.client-host example-host.tailnet-name.ts.net --yes
   pap settings set web-ui.enabled true --yes
   pap app open --web    # http://example-host.tailnet-name.ts.net:4182
   ```

   Use `--tcp`, not the default HTTPS proxy: that proxy presents an `https://`
   origin on port 443, which the service rejects. Remove the forwarder with
   `tailscale serve --tcp=4182 off`.

3. **Direct LAN listening.** Bind a LAN address only on a network you trust:

   ```sh
   pap settings set web-ui.allow-network-access true --yes
   pap settings set web-ui.listen-address 192.168.1.20 --yes
   # or every interface, with the name clients use:
   # pap settings set web-ui.client-host studio.local --yes
   # pap settings set web-ui.listen-address 0.0.0.0 --yes
   pap app open --web    # http://192.168.1.20:4182
   ```

   The connection is **unencrypted HTTP**. Anyone who can observe or
   intercept traffic on that network can read the password as it is sent, the
   session cookie and every page, including the Local API client key, and can
   act as the signed-in user. Use this only on a trusted network with a
   password you use nowhere else, and never expose the port to the internet,
   through port forwarding or otherwise. The desktop app's Settings page shows the
   same **Non-loopback** warning and asks for confirmation before saving.

Browsers treat plain HTTP on any address other than `127.0.0.1` as insecure, so
over Tailscale or a LAN the copy buttons are unavailable; select and copy text
instead.

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

`pap profiles use ID` exits `0` once the switch is applied and prints the
resulting status. While protection is on, it restarts on the new profile, so
that status may still be reconnecting or show why protection could not start
or verify. A profile without a credential is refused while protection is on,
and neither the active profile nor protection changes.

`--key-stdin` is for a pipe or redirected file, not a terminal. The production
OS requirement is a shared current policy, not a separate preference per profile;
`edit --require-production-os` restores the strict policy.

## Automation

Use `--json --non-interactive` for data and `--yes` only for changes you intend
to approve. These flags are independent: JSON and non-interactive mode never
imply consent. Results go to stdout; failures go to stderr. Watch mode emits
one JSON snapshot per line: the current state, then one per change (0.1
also repeated an unchanged snapshot every second). Human-readable output is
not a parsing contract.

`pap status --json` and `pap service start --json` report the backend as the
management API's `GET /api/version` answers it: `apiVersion`, `product`,
`version`, `instanceId`, `processId` and `executable` (0.1 named the first
field `protocolVersion`). A command that fails prints
`{"error": {"code": "CODE", "message": "MESSAGE"}}` on stderr. `error.code` is
the backend's stable error code, such as `invalid_state`, `busy` or
`revision_conflict`, or `command_failed` for a failure the CLI reports itself;
`error.message` is text for people. 0.1 always reported `command_failed` and
prefixed the message with the backend's code (`CODE: MESSAGE`). The desktop app
and the web UI receive the same `code` and `message`.

For reviewed agent changes, obtain a preview first:

```sh
pap --json agents connect codex --model MODEL --dry-run
pap --json --yes agents connect codex --model MODEL --revision REVISION
```

`pap --json agents list` reports `recorded: true` for every saved connection
and `connected: true` only while the agent's configuration routes it through
the Local API. A connection suspended because protection is off has its own
configuration restored: it reports `connected: false` (recorded but inactive)
until protection resumes.

Use the returned revision with the same agent, direction and options. A changed
configuration is rejected rather than silently re-previewed and approved.
Do not automatically retry mutations after a timeout or lost connection: they
may have completed. Query state before deciding what to do next.

Exit status is `0` for successful execution, `2` for invalid command arguments,
and `1` for operation failures. With `--json`, failures have an `error` object
with `code` and `message`. Argument errors use `invalid_arguments`; operation
errors use the backend's code, or `command_failed`. Classify failures by
`code`, never by parsing message text, and do not assume all failures are
retryable.

`doctor` reports every independent check, even when some fail. In that case it
prints the partial report on stdout and exits nonzero; the `errors` object
identifies failed checks. `settings` names the settings files; an invalid file
is an error. `warnings` lists problems that do not fail `doctor`: a
`credentials.toml` other users can read (`credentials`), profiles without a
saved API key (`profileCredentials`) and settings files left where macOS and
Windows builds up to 0.2.0-beta.8 kept them (`legacySettingsDirectory`; see
[Settings files](configuration.md#moving-from-the-earlier-macos-and-windows-location)). The `update` check is advisory: when the release
feed is unreachable it reports an `error` inside `update` without failing
`doctor`. `logs` is the directory of the service's log files.

## Logs

The background service writes its diagnostics to `service.<date>.log` in the
`logs` directory of the app data directory (`pap doctor` prints the path), one
file per day with the last seven kept, like Ollama's `~/.ollama/logs`. The
CLI and the desktop app print theirs on stderr. Only this app's own events are
logged, not its libraries'. The service log also records startup, every
protection status change and error, and each shutdown step. Logs never contain API keys,
tokens or the web UI password. The exported diagnostics report names the
directory with the home directory shown as `~`.

## Coverage

| Core capability | CLI |
| --- | --- |
| Backend and protection lifecycle | `service`, `start`, `stop [--offline]`, `status --watch` |
| Browser management UI | `settings set web-ui.enabled true`, `web-ui password show`/`rotate`, `settings set web-ui.password --value-stdin`, `web-ui.listen-address`/`web-ui.allow-network-access`/`web-ui.client-host`, `app open --web` |
| Profile inspection, verification and selection | `profiles list/show/add/edit/verify/use/remove` |
| Credential replacement and removal | `profiles verify --key-stdin`, `token clear-credential` |
| Agent configuration review and restoration | `agents list/connect/disconnect/disconnect-all` |
| Verified model catalog | `models list --refresh` |
| Usage, signed receipts, filtering, pagination, CSV and deletion | `usage list/show [--receipt]/export/clear` |
| Settings (`config.toml`) and its schema | `settings show/set/schema` |
| Local inference token | `token show/rotate` |
| Web UI password | `web-ui password show/rotate`, `settings set web-ui.password` |
| Configuration backups and redacted diagnostics | `profiles import/export`, `diagnostics` |
| CLI registration and app opening | `cli status/install/uninstall`, `app open` |
| Installation and connection diagnostics | `doctor` |

Usage time filters are Unix seconds. List pagination uses `--cursor` and
`--limit`; CSV export covers all records matching its filters, not just the
currently displayed page. Exports refuse existing destination files.

`usage show <id> --receipt` prints the signed receipt document (ACI spec §7.2)
the record's audit checked, byte for byte as the service returned it, to archive
or re-verify offline with `pap audit --receipt`. Receipts hold hashes and
verification metadata, never request or response content. They stay in the
local usage database, and `usage clear` deletes them with the records. An audit
reads at most 64 KiB of a receipt (the spec's test vector is under 1 KiB); a
larger document fails the audit without being checked or saved, so `--receipt`
reports that no receipt is saved, as it does before the audit finishes.

OS login startup, notification permissions and installer-based app updates stay
in the desktop app or the OS installer. `doctor` also reports whether the saved update
channel (`settings set update-channel beta|stable`) has a newer release and the
exact upgrade steps for this installation; see
[Updates by installation](distribution.md#updates-by-installation). Shared notification preferences are available
through `settings`; they do not grant OS notification permission.
