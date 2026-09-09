# Account login

Phala and RedPill profiles offer account login alongside manual API keys. Custom
endpoints use manual keys. Provider buttons with their icons stay in the form
content; authorization stages credentials, and the footer offers Cancel and
Verify and Save. Preset service endpoints are hidden. The runtime owns the
browser authorization, verification and OS credential store; the renderer only
receives presentation and non-secret account metadata.

## Authorization

Phala uses device authorization at `cloud-api.phala.com`, with
`client_id=private-ai-proxy`, `scope=redpill:api-key` and an installation UUID.
Polling honors pending, slowdown and expiry. Its returned token is an inference
key. Account metadata must load successfully before the authorization is ready.

RedPill uses public OAuth client `cGrHCOWG3S91oa0A` on `clerk.redpill.ai` with
exact callback `http://127.0.0.1:4181/oauth/callback`. Discovery must support code
flow, S256 PKCE and public token exchange. The callback checks Host, state, unique
code and issuer when present. Requests do not follow redirects. The requested
scopes are `openid profile user:org:read`; no client secret or refresh token is
used. Clerk tokens stay in runtime memory. Sign in alone does not issue a
RedPill inference key: Verify and Save exchanges the grant at
`POST https://service.redpill.ai/api/desktop/key`.

Organization selection is Clerk's OAuth extension, not an OAuth/OIDC standard.
The API reads the selected organization from trusted userinfo, checks current
membership and permissions, and provisions first-time users using the same
server-side projection as dashboard login. Opening the dashboard first is not
required. The API rechecks workspace access during issuance.

## Stable credentials and saving

Each device profile derives its installation UUID from a local installation ID
and the profile ID. The installation ID is not exported with profiles, so imports
on another machine do not share a device credential. Both providers issue one
managed key per actor, tenant and installation. Concurrent requests and lost
responses reuse the issued key; re-login preserves the key row, budgets and
usage. RedPill replaces the secret when reconnecting a disconnected row; Phala
reenables its existing upstream key. Administrative disable cannot be bypassed.
Keys are named Private AI Proxy in the provider console and can be managed there.

The server owns pending, active and disconnected states. Save acknowledges the
credential through activate; abort cannot disable an active key. Server workers
expire abandoned pending credentials after one hour, independently of UI
cancellation or process survival. Explicit revoke disconnects the key without
deleting its budget or usage history. Stored recovery secrets are encrypted
using each service's existing encryption configuration and never serialized in
API responses.

The runtime verifies the staged key before saving. A replacement is written to a
new OS credential-store entry, then the profile JSON atomically switches its
non-secret credential reference. Failure before that switch keeps the previous
credential selected. Account saves run independently of an individual IPC
request; the client polls an operation ID for a definite outcome. A concurrent
save receives an explicit busy failure. Saving reconnects protection only if it
was previously running.

Remote activation and retirement use a persistent retry queue. Each record has
its own OS credential-store item; an atomic, non-secret manifest holds only item
names. This avoids Windows credential blob size limits. Cleanup compares secrets
against every currently saved profile before revocation, so reselecting a stable
key cannot cause a stale queue entry to disable it. Activation precedes retirement.
Ordinary network failures retain records silently for bounded background retries;
unavailable activation is reported once and requires sign-in again.

Deleting a profile, clearing a credential, replacing an account key or changing
workspace queues retirement of the old managed key. Local removal works offline;
remote revocation completes after connectivity returns while the runtime is
running. Manual API keys are only removed locally. Cancelling staged login does
not revoke a saved credential. Unfinished first-time authorization is bounded by
server expiry. No operation revokes the user's Clerk OAuth grant.

## Release dependencies and validation

Deploy both provider changes before releasing desktop account login:

- [redpill-api #103](https://github.com/redpill-ai/redpill-api/pull/103): migration
  `98d712b3c4e5`, API and Celery worker/beat; existing `ENCRYPTION_KEY` configuration.
- [Phala #2138](https://github.com/Phala-Network/phala-cloud-monorepo/pull/2138):
  migration `proxy_device_credentials`, API and pending-key expiry worker;
  existing `WALLET_ENCRYPTION_PASSWORD` configuration.

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
`GET /api/desktop/account` returns only workspaces accessible to that actor in
that organization. One workspace is selected automatically; multiple workspaces
require an explicit choice in the app. Save sends that workspace ID and the API
rechecks its ownership and membership. The saved profile retains non-secret
organization/workspace names. Changing organization requires signing in again;
after a key has been issued, changing workspace also requires fresh authorization.

Check balance is available in the account section before Save and when editing a
saved account profile. The runtime uses the pending OAuth grant or the profile's
OS-stored inference key; credentials never pass through the renderer. RedPill
returns the organization's shared USD balance, independently of workspace/key
spending limits. An inference key needs a server-owned `desktop_balance_read`
grant, and its actor's live Clerk membership and billing-read permission are
checked again. Ordinary inference keys cannot use this endpoint. Keys created
before the balance grant was introduced require signing in and saving again.
Phala uses its existing `/api/v1/private_ai/self` contract and shows workspace
balance and promotional credits separately.

Top up opens the system browser at RedPill's `/credits` page or Phala's `/cost`
page, both of which include recharge controls. These sites use their own browser
session; the app shows which organization/workspace to select there. No invented
tenant query parameters, credentials in URLs, automatic checkout, or payment
mutation is used.
