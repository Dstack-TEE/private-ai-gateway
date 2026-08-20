# Minimal Control Plane

This directory contains a config-backed server for local middleware integration. It implements the smallest useful subset of the gateway's control-plane HTTP contract. It is not a production authorization, billing, catalog, or high-availability service.

See the complete [control-plane contract](../../docs/control-plane-contract.md) before implementing another control plane.

## Implemented behavior

The example exposes:

| Route | Behavior |
| --- | --- |
| `GET /` | Plain-text liveness response. |
| `GET /models` | Lists every configured model as an OpenAI-shaped catalog. |
| `POST /consult/pre` | Checks the optional API-key-hash allowlist, resolves a model, and returns its pricing and ordered candidates, including any route-specific reasoning dialect. |
| `POST /consult/post` | Parses and discards a usage report, then returns `{"ok":true}`. |

When `keys` is missing or empty, anonymous inference is allowed. When it contains hashes, `apiKeyHash` must match one of them.

The example does not implement:

- `/models/*` sub-catalogs;
- `/embeddings/models`;
- `tee=true` catalog filtering;
- `provider.only` or other provider-routing policy;
- special TEE-only denial behavior;
- rate limits, spending, durable usage ingestion, or idempotency;
- config reload;
- direct TLS or mutual TLS.

Do not use this server as the policy component of a production TEE-only hostname without implementing and testing those missing controls.

## Configure

Copy the example file:

```sh
cp control.config.example.json control.config.json
```

The schema is:

```json
{
  "keys": ["<lowercase sha256 of an accepted bearer token>"],
  "models": {
    "public-model": {
      "pricing": {
        "inputCostPerToken": "0.000001",
        "outputCostPerToken": "0.000002"
      },
      "candidates": [
        {
          "routeId": "upstream-name:public-model",
          "format": "openai",
          "engine": "vllm",
          "reasoningFormat": "reasoning_effort"
        }
      ]
    }
  }
}
```

`format` must be `openai` or `anthropic`. Optional `engine` must be `sglang` or
`vllm`. Optional `reasoningFormat` selects `reasoning_effort`, the nested
`reasoning` object, `chat_template_thinking`, or
`chat_template_enable_thinking`. When omitted, the gateway uses nested
`reasoning` for managed routes and `reasoning_effort` for routes with an
`engine`.

Declaring a reasoning format also lets the gateway interpret a caller's
`chat_template_kwargs` thinking switch. A `false` switch is re-encoded in the
declared upstream dialect. An explicit `reasoning` or `reasoning_effort` value
wins. A `true` switch is not translated because doing so would require the
gateway to invent an effort level.

Candidate route IDs must match the active gateway upstream config exactly:

```text
<upstream name>:<public model ID>
```

The server reads its config once at startup from `CONTROL_CONFIG_PATH`, defaulting to `/etc/pag/control.config.json`.

## Run

Node.js 18 or newer is required.

```sh
npm ci
npm run typecheck
npm run build

CONTROL_CONFIG_PATH=./control.config.example.json \
PRIVATE_AI_GATEWAY_CONTROL_PORT=8789 \
node build/server.js
```

Point the gateway at the server:

```json
{
  "middleware": {
    "control_url": "http://127.0.0.1:8789"
  }
}
```

The public gateway `GET /v1/models` request is relayed to this server as `GET /models`.

## Authenticate a remote connection

Set a bearer token on both sides:

```sh
export PRIVATE_AI_GATEWAY_CONTROL_TOKEN='<long-random-token>'
```

```json
{
  "middleware": {
    "control_url": "https://control.example",
    "control_token": "<same-token>"
  }
}
```

When the server variable is set, it protects `/consult/*` and `/models`. It does not protect the root liveness route. The server itself speaks plain HTTP, so a remote deployment must terminate authenticated TLS at a trusted proxy or run inside another protected transport.

The gateway denies inference with `503` when pre-consult times out, fails transport, returns non-200, or returns invalid JSON. Post-consult delivery is best effort and does not roll back a response already served.
