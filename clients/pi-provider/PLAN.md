# pi-provider-phala-cloud — Implementation Plan

A pi extension that wires Phala Cloud Confidential AI into pi, with per-response
verifiability as a first-class concern.

## What Phala Confidential AI provides

- **OpenAI-compatible API** at `https://inference.phala.com/v1` (`/chat/completions`,
  `/models`, streaming via `stream: true`). Auth: API key (`PHALA_LLM_API_KEY`,
  separate from the CVM-management `PHALA_CLOUD_API_KEY`), no OAuth.
- **Per-response verifiability.** Every response carries three headers:
  - `x-receipt-id` — signed receipt id
  - `x-aci-identity` — attested gateway workload identity
  - `x-aci-keyset-digest` — digest of the attested gateway keyset
- **Verification surface:**
  - `GET /v1/aci/receipts/{id}` — signed receipt with ordered `event_log`
    (`upstream.verified` carries `result`/`required`/`provider`/`model_id`/
    `session_id`/`channel_bindings`/`claims`).
  - `GET /v1/aci/attestation?nonce=` — gateway TEE attestation report
    (`workload_id`, `workload_keyset_digest`, signing keys, provenance, freshness).
  - `GET /v1/aci/sessions/{id}` — immutable verified upstream session with
    typed claims and channel binding.
- **Model discovery.** `/v1/models` returns `is_tee`, `context_length`,
  `pricing`, `input_modalities`, `supported_parameters`.

## How pi provider extensions work

`package.json` declares `"pi": { "extensions": ["./index.ts"] }`. Pi loads
TypeScript directly (no build step). The factory registers a provider via:

```ts
pi.registerProvider("phala-cloud", {
  baseUrl, apiKey: "$PHALA_LLM_API_KEY", api: "openai-completions",
  models: ProviderModelConfig[], headers?, authHeader?,
  streamSimple?, oauth?
});
```

Extensions can also: register commands, register tools, subscribe to events
(`after_provider_response` exposes response headers; `before_provider_request`
exposes payload; `message_end` fires when streaming finishes), drive the footer
via `ctx.ui.setStatus(key, text)`, and render a `SettingsList` config UI.

## Decisions (confirmed)

1. **Package name**: `pi-provider-phala-cloud`. **Provider id**: `phala-cloud`.
2. **No custom `streamSimple`** for MVP — use the built-in `openai-completions`
   handler + event hooks. Thinking works because pi's built-in handler supports
   `compat.thinkingFormat: "qwen"` (sends `enable_thinking`) and parses
   streaming `reasoning_content` into pi thinking blocks. Phala's Qwen3 models
   accept `enable_thinking` natively.
3. **E2EE not implemented**, but hooks are reserved so a future E2EE pass can
   plug in without reworking the architecture:
   - `src/headers.ts` `buildPhalaHeaders()` — the single place request headers
     are assembled. Returns `{}` today; E2EE injects `X-E2EE-*` /
     `X-Client-Pub-Key` here.
   - `src/models.ts` keeps model registration behind one function; switching to
     `streamSimple` (needed for field-level payload encryption) is a localized
     change.
   - `src/verify.ts` isolates receipt interpretation; E2EE changes
     `request.received.body_hash` semantics (hash of decrypted body), handled
     there.
   - `src/config.ts` reserves an `e2ee` config block (default disabled).
4. **`is_tee` filtering is configurable.** `phala-cloud-settings` toggles
   `models.isTeeOnly` (default `true`). Config supports home + project scope.
5. **Footer shows verification result** after each response.
6. **No OAuth** (server does not support it yet).

## Architecture

```
index.ts  PhalaCloud() factory
  |
  +-- registerProvider("phala-cloud", { openai-completions, models, headers })
  |     models <- src/models.ts  discoverPhalaModels() /v1/models
  |                                  -> is_tee filter, thinkingFormat inference,
  |                                     pricing/context/maxTokens mapping
  |     headers <- src/headers.ts  buildPhalaHeaders()  [E2EE hook point]
  |
  +-- on("session_start")      -> re-discover models, re-register
  +-- on("after_provider_response") -> capture x-receipt-id/aci headers
  |                                     -> setStatus "attested" | "—"
  +-- on("message_end")        -> async fetch receipt -> classify
  |                                     -> setStatus "verified" | "routed" | "?"
  +-- registerCommand("phala-cloud-settings") -> SettingsList (home/project)
```

## File layout

```
package.json  tsconfig.json  .oxlintrc.json  .oxfmtrc.json
.pre-commit-config.yaml  .gitignore  README.md  PLAN.md
index.ts
src/
  constants.ts      baseUrl, PROVIDER_ID, version, env-driven config
  config.ts         layered config (default/home/project/env/runtime) + sources
  project-trust.ts  project trust gate for project-scope config
  headers.ts        request header builder [E2EE hook point]
  models.ts         /v1/models discovery -> ProviderModelConfig[], thinking map
  verify.ts         receipt/attestation fetch + workload match + classify
  receipt-store.ts  last receipt + cached attestation (stale_after expiry)
  settings-ui.ts    SettingsList helpers
tests/
  models.test.ts    thinkingFormat inference, is_tee filter, pricing mapping
  config.test.ts    layered merge + validate
  verify.test.ts    workload match + receipt classify
```

## Phased delivery

| Phase | Scope | Done when |
|-------|-------|-----------|
| P0 | Scaffold + provider passthrough + model discovery + thinking | `PHALA_LLM_API_KEY=... pi -e ./pi-provider-phala-cloud` chats on a phala model with thinking |
| P1 | Footer verification | Each response updates footer to verified/routed/attested |
| P2 | `phala-cloud-settings` command (isTeeOnly + thinkingFormat, home/project) | Toggle persists, `/reload` applies |
| P3 | `/phala-verify` + `/phala-attestation` commands (receipt + session claims) | Commands print event_log key fields |
| P4 | E2EE (future) | Switch to streamSimple, inject headers, decrypt fields |

Current focus: **P0 + P1** (chat with thinking + footer verify).

## Known limits (MVP)

- **Response `wire_hash` cannot be verified inside the extension.**
  `after_provider_response` exposes `status` + `headers` only, not response
  bytes. We verify workload identity match + `upstream.verified` semantics;
  wire_hash is surfaced for external/manual verification.
- **Request `body_hash` is best-effort** (JSON serialization order may differ
  from the gateway's observed bytes). Default off.
- **Receipt signature verification** (ecdsa-secp256k1) deferred to a later
  phase; MVP checks workload_id/keyset_digest matching against a cached
  attestation.
