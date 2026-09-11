# Account login

Phala and RedPill profiles offer account login alongside manual API keys. Custom
endpoints use manual keys. Provider buttons with their icons stay in the form
content. Phala completes automatically after browser authorization, which already
selects its workspace. RedPill displays a standard workspace selector and Save,
including when only one workspace is available. Failed persistence
keeps the authorization available through Retry, without signing in again.
Manual keys and ordinary profile edits use Save. Persistence does not start gateway verification. Preset service endpoints are hidden. The runtime owns the
browser authorization, verification and OS credential store; the renderer only
receives presentation and non-secret account metadata.

## Authorization

Phala uses device authorization at `cloud-api.phala.com`, with
`client_id=private-ai-proxy` and `scope=redpill:api-key`.
Polling honors pending, slowdown and expiry. Its returned token is an inference
key. Account metadata must load successfully before the authorization is ready.

RedPill uses public OAuth client `cGrHCOWG3S91oa0A` on `clerk.redpill.ai` with
exact callback `http://127.0.0.1:4181/oauth/callback`. Discovery must support code
flow, S256 PKCE and public token exchange. The callback checks Host, state, unique
code and issuer when present. Requests do not follow redirects. The requested
scopes are `openid profile user:org:read`; no client secret or refresh token is
used. Clerk tokens stay in runtime memory. Once the workspace is resolved,
the runtime exchanges the grant for an inference key at
`POST https://service.redpill.ai/api/oauth/key`.

Organization selection is Clerk's OAuth extension, not an OAuth/OIDC standard.
The API reads the selected organization from trusted userinfo, checks current
membership and permissions, and provisions first-time users using the same
server-side projection as dashboard login. Opening the dashboard first is not
required. The API rechecks workspace access during issuance.

## Stable credentials and saving

Phala uses ordinary inference keys. Each authorization creates a new key named
with its `client_id`. There is no activation, remote cancellation, automatic key
expiry or installation binding. Cancelling login, replacing a key or removing a
profile only discards local credentials; unused keys must be managed in the Phala
dashboard. New keys do not inherit the previous key's budget.

RedPill device profiles derive an installation UUID from the local installation
and profile IDs. RedPill owns its managed credential lifecycle, including activation,
revocation and expiry of abandoned pending credentials. The retry queue below
applies to RedPill only.

Save validates the configuration and credential format. A replacement is written to a
new OS credential-store entry, then the profile JSON atomically switches its
non-secret credential reference. Failure before that switch keeps the previous
credential selected. Account saves run independently of an individual IPC
request; the client polls an operation ID for a definite outcome. A concurrent
save receives an explicit busy failure. Saving reconnects protection only if it
was previously running.

RedPill activation and retirement use a persistent retry queue. Each record has
its own OS credential-store item; an atomic, non-secret manifest holds only item
names. This avoids Windows credential blob size limits. Cleanup compares secrets
against every currently saved profile before revocation, so reselecting a stable
key cannot cause a stale queue entry to disable it. Activation precedes retirement.
Ordinary network failures retain records silently for bounded background retries;
unavailable activation is reported once and requires sign-in again.

Deleting a RedPill profile, clearing its credential, replacing its account key or
changing workspace queues retirement of the old managed key. Local removal works offline;
remote revocation completes after connectivity returns while the runtime is
running. Phala keys and manual API keys are only removed locally. Cancelling staged login does
not revoke a saved credential. Aborting an unsaved issued key is best effort;
a failed request never prevents closing the editor. Unfinished authorization is bounded by
server expiry. No operation revokes the user's Clerk OAuth grant.

## Release dependencies and validation

RedPill requires its managed-credential backend; Phala uses its existing device flow:

- [redpill-api #103](https://github.com/redpill-ai/redpill-api/pull/103): migration
  `98d712b3c4e5`, API and Celery worker/beat; existing `ENCRYPTION_KEY` configuration.
  Configure `OAUTH_CLIENTS` to allow public client `cGrHCOWG3S91oa0A` with label
  `Private AI Proxy`. The shared `/api/oauth` API isolates credentials per client.
- [Phala #2138](https://github.com/Phala-Network/phala-cloud-monorepo/pull/2138):
  client-based key naming only; no new migration, lifecycle endpoint or worker.

The Clerk public app is registered, but registration does not deploy the API
endpoints. No provider credentials belong in renderer state, profile JSON,
command arguments, agent configuration or logs.

Focused checks cover real PostgreSQL concurrency and migrations, provisioning,
permission failures, budget preservation, pending expiry, local HTTP authorization
contracts, offline deletion, stable-key reselection, save concurrency and the
renderer flow. Packaged OS credential-store checks and real consent through
inference and billed usage remain release acceptance checks.

## Organization, workspace and billing

RedPill selects the organization in Clerk's OAuth consent screen. After sign-in,
`GET /api/oauth/account` returns only workspaces accessible to that actor in
that organization. One workspace is preselected; multiple workspaces require a
choice. Save sends that workspace ID and the API rechecks its ownership and membership. The saved profile retains non-secret
organization/workspace names. Changing organization requires signing in again;
saved RedPill profiles load their workspace options using their managed key.
Choosing another workspace and clicking Save starts fresh authorization, then
finishes that same save if the chosen workspace remains available. Cancellation
or failure preserves the saved profile. Deploy the API's managed-key account
read support before releasing this editor change. Phala
has no separate organization tier: its workspace (team) is selected in the browser.

Balances load automatically after authorization, when opening a saved account,
and on the active profile's main card. The main card adds only a current-balance
button beside the profile selector; clicking it opens the provider billing page.
Refresh remains in the account actions menu. Organization,
workspace, promotional credits and Top up remain in the editor. The same component handles both surfaces,
refreshing a visible main card every five minutes, an editor every minute, and
on focus with a 30-second minimum interval. The runtime coalesces concurrent windows by login ID or profile ID plus
credential reference; successful results live for 30 seconds and failures for 10.
A replaced credential cannot reuse an old cache entry. Balance reads do not update
profile settings or hold the authorization lock during network requests. The renderer discards results when the target changes.

Loading and unavailable states never masquerade as a zero balance. Refresh and
Top up remain separate actions; balance failures stay local to this display and
cannot interrupt protection. RedPill shows the shared organization USD balance;
workspace and key limits still apply. Saved app keys require current live Clerk
billing-read and workspace permissions, so a later permission grant no longer
requires another sign-in. The API limits balance requests to ten per minute per
bearer credential across workers using Redis; Redis failure only makes balance
unavailable. Phala shows workspace balance and promotional credits separately.

There is one authorization session per runtime, not per account. Repeated clicks
are blocked in the hook; another window receives an explicit instruction to
finish or cancel the existing sign-in for a different profile. Reopening the same
profile replaces its pending session, including one left by a closed native window.
A save or balance operation cannot make a
second begin request wait and unexpectedly launch another flow afterward. Users
can intentionally create separate profiles for the same account. Browser OAuth
success is not gateway verification: the runtime still verifies the selected
provider when starting or reconnecting protection. Saving alone never claims the
provider is verified and never silently changes the selected tenant.

Top up opens the system browser at RedPill's `/credits` page or Phala's `/cost`
page, both of which include recharge controls. These sites use their own browser
session; the app shows which organization/workspace to select there. No invented
tenant query parameters, credentials in URLs, automatic checkout, or payment
mutation is used.


## Callback fallback and CLI

The browser callback uses the app's generated brand mark, system typography and
light/dark appearance. It says Authorization received, not Connected: token
exchange and local saving may still be pending. The page never echoes the code,
state or error details; it has no scripts or external assets and sends no-store,
no-referrer and restrictive CSP headers.

If the automatic loopback redirect cannot reach this machine, expand Paste
callback link in the RedPill account panel. Paste the complete URL from the
browser. The runtime accepts only the registered callback origin/path, current
state and optional issuer, using the same one-shot receiver and PKCE exchange as
the HTTP callback. Wrong, expired and reused callbacks are rejected. The temporary
password input is cleared immediately on submission and is not persisted. Copy
sign-in link is available if the system browser launcher fails.

CLI account login shares the runtime authorization and save state:

```sh
pap profiles login work --provider redpill
pap profiles login personal --provider phala
pap profiles login work --provider redpill --workspace 123 --no-browser --callback-stdin
pap start --profile work
```

Login prints/opens the authorization URL. Multiple workspaces prompt in an
interactive terminal; automation supplies --workspace. RedPill callback fallback
reads a hidden terminal prompt or a bounded stdin stream, never a command-line
credential argument. No-browser supports remote terminals. CLI JSON mode returns
non-secret profile state; login links/prompts go to stderr. A lost save response
is reconciled by operation ID. Explicit pap profiles verify remains available;
normal UI Save does not verify. Starting protection always performs attestation
and connection checks before exposing inference to agents.

## Account identity and billing permissions

The organization card displays its name, avatar and permitted billing information
using official shadcn Item and Avatar components. Personal identity is not rendered.
Its menu contains only Manage (the organization's console) and Switch (fresh
authorization). The larger organization avatar, vertically centered name and
right-aligned balance share one row. Balance refresh is automatic; the editor
has no Top up or Refresh balance action. Manage uses the organization ID from the
account response and remains available without billing-read permission. RedPill names and avatar URLs come from the API's
verified Clerk membership response; opening the editor refreshes saved identity
metadata. Missing or failed avatars fall back to initials. Only HTTPS Clerk image
hosts are accepted, allowed explicitly by CSP, and images send no referrer.

OAuth scopes remain `openid profile user:org:read`. Key management, billing read
and billing management are separate live organization permissions. A denied
balance response becomes a typed null result: no balance or Top up is rendered.
A billing reader sees the balance but no Top up unless management is permitted.
Permission changes are rechecked by the API on refresh, subject to the existing
30-second runtime cache. Network failures remain errors, not zero balances or
permission denials. Balance request failures hide the balance entry and retry on
the normal refresh schedule; they do not add a form error. RedPill account and
balance responses require organization IDs and slugs; balance responses also
require explicit billing permissions. Billing links use
`/{organizationSlug}/credits`,
so the browser's previously selected organization cannot redirect the user's
intended billing scope. The billing website still authorizes all mutations.

Manage opens `/{organizationSlug}`. Account and balance responses must supply
`organization_slug` for these links; missing slugs do not fall back to another tenant.
