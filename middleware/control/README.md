# Reference control plane (open)

This is a **minimal, config-driven** implementation of the gateway's control
plane — the content-blind decision plane the executor consults. It exists so the
open stack runs end-to-end and so the executor↔control contract has a working,
testable example. See [`../../docs/control-contract.md`](../../docs/control-contract.md).

It is **not** the production control. The real control (auth against a database,
profit-EV routing, rate limiting, billing/spend, metrics) is a separate,
closed-source service published as its own image. Because integration is purely
the HTTP-over-UDS contract + a digest-pinned image, that proprietary control is a
**drop-in replacement**: swap the `control` image in `deploy/docker-compose.yml`
and supply its database env.

## What it does

- `GET /models` — lists the models from the config.
- `POST /consult/pre` — `{apiKeyHash?, model}` → allow/deny + pricing + ordered
  route candidates, all from the config. Denies unknown models; if `keys` is
  non-empty it requires the request's `apiKeyHash` to be in the list (empty list
  = anonymous allowed).
- `POST /consult/post` — accepts the usage report and drops it (no billing).

No Postgres / Redis / ClickHouse.

## Config

Reads JSON from `CONTROL_CONFIG_PATH` (default `/etc/pag/control.config.json`).
See [`control.config.example.json`](./control.config.example.json). The image
bakes the example as the default; mount your own to override.

## Run

```bash
npm install && npm run build
CONTROL_CONFIG_PATH=./control.config.example.json \
PRIVATE_AI_GATEWAY_CONTROL_UDS_PATH=/run/pag/control.sock \
node build/server.js
```
