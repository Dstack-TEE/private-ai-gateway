# Control plane

A **minimal, config-driven** implementation of the gateway's control plane — the
decision plane the executor consults. It exists so the stack runs end-to-end and
gives a working, testable example of the executor↔control HTTP surface (the three
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

The control plane listens on a TCP port; the executor reaches it over HTTP(S) at
`PRIVATE_AI_GATEWAY_CONTROL_URL`.

```bash
npm install && npm run build
CONTROL_CONFIG_PATH=./control.config.example.json \
PRIVATE_AI_GATEWAY_CONTROL_PORT=8789 \
node build/server.js
```

Then run the executor with `PRIVATE_AI_GATEWAY_CONTROL_URL=http://127.0.0.1:8789`.

## Remote mode

The control plane can run on a separate host that the executor reaches over the
network. The consult payloads carry only `{apiKeyHash, model}` and usage counts.

- **Authentication** — set `PRIVATE_AI_GATEWAY_CONTROL_TOKEN`. When set, the
  control enforces `Authorization: Bearer <token>` on `/consult/*` and `/models`;
  the executor sends it via its own `PRIVATE_AI_GATEWAY_CONTROL_TOKEN`. Unset =
  local dev, no auth.
- **TLS** — terminate TLS at a reverse proxy in front of this process (the
  executor dials `https://…`). The process itself speaks plain HTTP + token, so
  the code change stays minimal; optional hardening is direct TLS / mTLS.
- **Availability** — the executor fails **closed** (503) if the control is
  unreachable, since the pre-request consult gates authorization. Deploy it near
  the gateway, with HA; the executor holds a keep-alive connection.
