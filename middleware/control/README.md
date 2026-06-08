# Control plane

A **minimal, config-driven** implementation of the gateway's control plane — the
content-blind decision plane the executor consults over a Unix socket. It exists
so the stack runs end-to-end and gives a working, testable example of the
executor↔control HTTP surface (the three endpoints below).

It is **not** the production control, which is a separate, closed-source service
published as its own image. Because the executor talks to it only over these
HTTP-over-UDS endpoints, the production control is a **drop-in replacement**:
swap the `control` image in the deployment compose and supply its env.

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
See [`control.config.example.json`](./control.config.example.json). The image
bakes the example as the default; mount your own to override.

## Run

```bash
npm install && npm run build
CONTROL_CONFIG_PATH=./control.config.example.json \
PRIVATE_AI_GATEWAY_CONTROL_UDS_PATH=/run/pag/control.sock \
node build/server.js
```
