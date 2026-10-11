# Minimal Control Plane

This directory contains a config-backed server for local middleware integration. It implements the smallest useful subset of the gateway's control-plane HTTP contract. It is not a production authorization, billing, catalog, or high-availability service.

See the complete [control-plane contract](../../docs/control-plane-contract.md) before implementing another control plane.

## Implemented behavior

The example exposes:

| Route | Behavior |
| --- | --- |
| `GET /` | Plain-text liveness response. |
| `GET /models` | Lists every configured model as an OpenAI-shaped catalog. |
| `POST /consult/pre` | Checks the optional API-key-hash allowlist, resolves a model, and returns its pricing and ordered candidates, including any route-specific reasoning dialect. Denies a missing model with `400`, a key outside the allowlist with `401`, and an unknown model with `404`. |
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
          "reasoningFormat": "reasoning_effort",
          "supportedEndpoints": ["/v1/responses"]
        }
      ]
    }
  }
}
```

The server returns `pricing` and `candidates` as written and does not validate
them. Write them to the
[pre-consult response fields](../../docs/control-plane-contract.md#post-consultpre)
the gateway reads. Each `routeId` must match an `<upstream name>:<public model ID>`
route in the active gateway upstream config exactly.

The server reads its config once at startup from `CONTROL_CONFIG_PATH`, defaulting to `/etc/pag/control.config.json`.

## Run

Node.js 18 or newer is required.

```sh
npm ci
npm run typecheck
npm run build

CONTROL_CONFIG_PATH=./control.config.json node build/server.js --port=8789
```

The server listens on port 8789 by default. `--port=<n>` takes precedence over
`PRIVATE_AI_GATEWAY_CONTROL_PORT`.

Point the gateway at the server:

```json
{
  "middleware": {
    "control_url": "http://127.0.0.1:8789"
  }
}
```

The public gateway `GET /v1/models` request is relayed to this server as `GET /models`.

## Data boundary

The control plane never receives prompts or responses, but it does receive
request metadata; [What leaves the request path](../../docs/attested-confidential-inference.md#what-leaves-the-request-path)
lists every field.

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
