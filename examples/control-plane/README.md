# Control plane

A **minimal, config-driven** implementation of the gateway's control plane — the
decision plane the gateway consults. It exists so the stack runs end-to-end and
gives a working, testable example of the gateway↔control HTTP surface (the three
endpoints below).

## What it does

- `GET /models` — lists the models from the config.
- `POST /consult/pre` — `{apiKeyHash?, model}` → allow/deny + pricing + ordered
  route candidates, all from the config. Denies unknown models; if `keys` is
  non-empty it requires the request's `apiKeyHash` to be in the list (empty list
  = anonymous allowed).
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
network. The consult payloads carry only `{apiKeyHash, model}` and usage counts.

- **Authentication** — set `PRIVATE_AI_GATEWAY_CONTROL_TOKEN` on the control. When
  set, it enforces `Authorization: Bearer <token>` on `/consult/*` and `/models`;
  the gateway sends it via `middleware.control_token`. Unset = local dev, no auth.
- **TLS** — terminate TLS at a reverse proxy in front of this process (the
  gateway dials `https://…`). The process itself speaks plain HTTP + token, so
  the code change stays minimal; optional hardening is direct TLS / mTLS.
- **Availability** — the gateway fails **closed** (503) if the control is
  unreachable, since the pre-request consult gates authorization. Deploy it near
  the gateway, with HA.
