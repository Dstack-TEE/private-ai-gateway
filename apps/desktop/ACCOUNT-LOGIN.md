# Account login

Phala and RedPill profiles offer account login alongside manual API keys. Custom
endpoints use manual keys. The runtime owns authorization, key verification, and
persistence. Sign in stays in the form content and only stages authorization;
Verify and Save is a separate footer action. The Tauri shell only opens the browser and the renderer receives
non-secret presentation and account metadata. Each runtime allows one login at a
time, with a 15-minute deadline and explicit cancellation.

The renderer's `useAccountLogin` owns polling, cancellation and unmount cleanup;
the form owns its draft and Save action. `PendingLogin` owns the runtime task,
expiry, provider binding and staged credential through exclusive authorizing,
authorized and failed states. The controller only orchestrates verification,
persistence and reconnect. A failed authorization can be polled again safely;
failed key revocation retains the session so cleanup can be retried.

Phala uses the existing provider device flow at `cloud-api.phala.com`, with
`client_id=private-ai-proxy` and `scope=redpill:api-key`. Its token endpoint long
polls for up to 25 seconds; the client allows 35 seconds per request and honors
pending, slowdown and expiry. The returned access token is an inference key.
The production start endpoint accepted this client ID on 2026-09-09.

RedPill uses public OAuth client `cGrHCOWG3S91oa0A` on `clerk.redpill.ai`. The
registered callback is exactly `http://127.0.0.1:4181/oauth/callback`; no wildcard
ports or client secret are used. Discovery must support authorization code,
S256 PKCE and public token exchange. The callback checks the exact Host, state,
unique code, and issuer when present. Requests cannot follow redirects to other
origins. The app requests `openid profile user:org:read`, not offline access;
Clerk tokens stay in memory until the explicit Verify and Save action. Signing
in alone never calls the key exchange, so cancelling re-login cannot rotate an
existing RedPill key.

`POST https://service.redpill.ai/api/desktop/key` exchanges that token for a
virtual key. The API checks the client, scopes, user, selected organization,
current membership permissions and default-workspace access. A stable UUID derived
from the profile ID identifies this device profile. Re-login rotates the same row
without resetting its budgets or usage. The key remains visible and manageable
in RedPill Keys under `Private AI Proxy — <profile name>`.

After authorization, the form shows the signed-in account and stays open. Only
Verify and Save invokes key issuance, verification and persistence of the key
and OAuth profile together. Phala already returns an inference key during its
device flow; that key stays in runtime memory until Save. Cancelling or changing
the provider discards the pending authorization. Saving a running profile reconnects protection;
saving while stopped leaves protection stopped. Failed verification retains the staged credential for retry. Cancelling an
unsaved, already-issued RedPill key uses the self-revocation endpoint. Revocation
failures are shown with a dashboard recovery action. Phala has no verified
key-authenticated revocation endpoint; issued keys can be removed in its console.

Profile deletion retains its existing **local-only** meaning. It is not a Clerk
grant revocation. A cancellation or process failure racing server-side key
issuance can leave a key in the provider console; the server-side installation
identity bounds RedPill re-login to one key row per profile/workspace. Removing a
profile and creating a new one creates a new device identity.

## Release dependency

Deploy [redpill-api #103](https://github.com/redpill-ai/redpill-api/pull/103) before releasing the desktop login UI.
The Clerk public app is registered, but registration alone does not make the new
exchange endpoint available. No Clerk, Phala, or inference credentials belong in
the renderer, profile JSON, command arguments, or agent configuration.

Local fixture checks cover the callback trust boundary, token/client/scope and
membership rejection, budget-preserving replacement, plus the real profile UI
with a simulated provider. Full authorization and OS credential-store acceptance
on packaged macOS/Windows/Linux apps remain release checks.

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
