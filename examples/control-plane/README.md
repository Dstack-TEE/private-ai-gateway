# Control plane

A **minimal, config-driven** implementation of the gateway's control plane — the
decision plane the gateway consults. It exists so the stack runs end-to-end and
gives a working, testable example of the gateway↔control HTTP surface (the three
endpoints below).

## What it does

- `GET /models` — lists the models from the config.
- `POST /consult/pre` — `{apiKeyHash?, model, reasoning?}` → allow/deny + pricing
  + ordered route candidates, all from the config. Denies unknown models; if
  `keys` is non-empty it requires the request's `apiKeyHash` to be in the list
  (empty list = anonymous allowed). Reasoning-aware routing is optional and this
  minimal example does not implement it. A candidate can return
  `effectiveReasoning` to override the normalized request. Candidates can set
  `reasoningFormat` to `"reasoning_effort"`, `"reasoning"`,
  `"chat_template_thinking"`, `"chat_template_enable_thinking"` or
  `"thinking_type"` (DeepSeek's `thinking: {"type": ...}` switch, with
  `reasoning_effort` as the level) to select the upstream parameter dialect
  explicitly. When omitted, the gateway preserves
  its legacy behavior: managed routes use nested `reasoning`, while SGLang and
  vLLM routes use `reasoning_effort`.

  Set `nativeResponses: true` on a candidate whose upstream serves
  `/v1/responses` directly; otherwise the gateway converts through Chat
  Completions.

  Declaring a dialect also opts the route into reading a caller's
  `chat_template_kwargs` thinking switch. Some callers can only express "no
  thinking" that way, and left untranslated the switch reaches the upstream as
  an opaque key that only a real vLLM/SGLang server acts on — a vendor API
  behind the same route ignores it and keeps thinking. On a route that declares
  a dialect, a switch set to `false` is re-encoded in that dialect. An explicit
  `reasoning`/`reasoning_effort` still wins, and a switch set to `true` is never
  translated: encoding "on" would have to invent an effort the caller did not
  ask for.
- `POST /consult/post` — accepts the usage report and drops it (no billing).

No database; configuration only.

## Config

Reads JSON from `CONTROL_CONFIG_PATH` (default `/etc/pag/control.config.json`).
See [`control.config.example.json`](./control.config.example.json).

## Run

The control plane listens on a TCP port; the gateway reaches it over HTTP(S) at
the `middleware.control_url` from its static config.

```bash
npm install && npm run build
CONTROL_CONFIG_PATH=./control.config.example.json \
PRIVATE_AI_GATEWAY_CONTROL_PORT=8789 \
node build/server.js
```

Then point the gateway at it by setting `middleware.control_url` to
`http://127.0.0.1:8789` in the static gateway config.

## Remote mode

The control plane can run on a separate host that the gateway reaches over the
network. The consult payloads carry only request metadata, including
`apiKeyHash`, `model`, optional normalized `reasoning`, and usage counts.

- **Authentication** — set `PRIVATE_AI_GATEWAY_CONTROL_TOKEN` on the control. When
  set, it enforces `Authorization: Bearer <token>` on `/consult/*` and `/models`;
  the gateway sends it via `middleware.control_token`. Unset = local dev, no auth.
- **TLS** — terminate TLS at a reverse proxy in front of this process (the
  gateway dials `https://…`). The process itself speaks plain HTTP + token, so
  the code change stays minimal; optional hardening is direct TLS / mTLS.
- **Availability** — the gateway fails **closed** (503) if the control is
  unreachable, since the pre-request consult gates authorization. Deploy it near
  the gateway, with HA.
