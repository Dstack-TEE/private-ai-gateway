# Configure a Private Chutes Route

Private Chutes routes use the standard `chutes` adapter, a dedicated chute origin, pinned chute identifiers, and encrypted `/e2e/invoke` transport.

## Add the upstream

Add an entry to the upstream seed or active runtime configuration:

```json
[
  {
    "name": "private-chutes-model",
    "provider": "chutes",
    "base_url": "https://private-chute.example.chutes.ai",
    "models": {
      "private/model": "Provider/model-id"
    },
    "bearer_token": "<admin-scoped-private-chute-credential>",
    "basic_auth": true,
    "chutes_e2ee_api_base": "https://api.chutes.ai",
    "chutes_chute_ids": {
      "Provider/model-id": "00000000-0000-0000-0000-000000000000"
    },
    "session_refresh_seconds": 45,
    "chutes_e2ee_discovery_rounds": 3
  }
]
```

The credential must be authorized to retrieve the private chute's instance and attestation evidence as well as invoke it. The gateway returns only a redacted credential indicator from the admin API.

## Field behavior

| Field | Requirement |
| --- | --- |
| `base_url` | Dedicated chute origin used for model traffic. |
| `models` | Maps each public gateway alias to the provider model identifier. |
| `bearer_token` | Complete scoped credential. Required for Chutes evidence discovery. |
| `basic_auth` | Set to `true` when the private origin and Chutes evidence calls expect Basic authentication. |
| `chutes_e2ee_api_base` | Central discovery, evidence, and encrypted invocation origin. Defaults to `https://api.chutes.ai`. |
| `chutes_chute_ids` | Maps each configured provider model identifier to its chute UUID. Each key must appear in `models` values. |
| `session_refresh_seconds` | Refresh cadence for the verified single-use nonce pool. Defaults to 45 seconds for Chutes. |
| `chutes_e2ee_discovery_rounds` | Evidence discovery attempts, from 1 to 10. Defaults to 3. |

`basic_auth` requires `bearer_token` and is allowed only for `openai-compatible` and `chutes` routes. Chutes-specific fields on another provider type are rejected.

## Verify before sending data

Survey the current sessions:

```sh
curl --fail --silent --show-error \
  'http://127.0.0.1:8086/v1/aci/sessions?model=private%2Fmodel' | jq
```

Then require a verified route on inference:

```json
{
  "model": "private/model",
  "messages": [{"role": "user", "content": "Hello"}],
  "provider": {"aci_verified": true}
}
```

The gateway verifies instance evidence, restricts selection to the attested E2EE keys, encrypts the provider request, and decrypts the response. Read [Chutes verification](verification.md) before deciding which TCB and GPU claim states to accept.
