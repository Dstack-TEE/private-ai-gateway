# Private Chutes configuration

Private Chutes routes use the Chutes provider adapter, verified instance evidence,
and encrypted `/e2e/invoke` transport.

## Upstream configuration

Add an entry like this to `<state_dir>/upstreams.json`:

```json
[
  {
    "name": "private-chutes-uncensored-24b",
    "provider": "chutes",
    "base_url": "https://phala-agent-askvenice-venice-uncensored.chutes.ai",
    "models": {
      "phala/uncensored-24b": "AskVenice/venice-uncensored"
    },
    "bearer_token": "<admin-scoped-private-chute-credential>",
    "basic_auth": true,
    "chutes_e2ee_api_base": "https://api.chutes.ai",
    "chutes_chute_ids": {
      "AskVenice/venice-uncensored": "28d17d83-7036-5a8c-8ca1-f148c126bd89"
    }
  }
]
```

## Field mapping

- `base_url` is the dedicated Chute origin.
- `models` maps the gateway's public model id to the provider-facing Chutes model id.
- `bearer_token` contains the complete scoped credential. An admin-scoped credential
  allows the verifier to retrieve attestation evidence as well as invoke the Chute.
- `basic_auth: true` applies Basic authentication to discovery, evidence retrieval,
  and encrypted `/e2e/invoke` requests.
- `chutes_e2ee_api_base` defaults to `https://api.chutes.ai`; keeping it explicit
  makes the central E2EE control-plane endpoint visible in the deployment config.
- Each `chutes_chute_ids` key matches a provider-facing model id from `models`.
  Its UUID value pins that model to a specific private Chute.

The gateway verifies the instance E2EE key against Chutes attestation evidence,
encrypts the request with ML-KEM, invokes `/e2e/invoke`, and decrypts the response.
See [verification.md](verification.md) for the complete trust and binding model.
