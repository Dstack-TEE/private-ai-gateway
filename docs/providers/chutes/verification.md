# Chutes Verification

The Chutes adapter verifies each discovered TDX instance and encrypts provider traffic to that instance's attested ML-KEM public key.

| Property | Current behavior |
| --- | --- |
| Attestation scope | Per instance |
| Verifier | `scripts/provider_verifier/chutes.py` through the provider-verifier bridge |
| Verifier ID | `private-ai-verifier/chutes/v1` |
| Enforced binding | `e2ee_public_key_sha256` |
| Transport | Encrypted `/e2e/invoke` request and response |

See [Private Chutes configuration](configuration.md) for dedicated origins and chute-ID pins.

## Verification algorithm

The adapter resolves the model's chute, discovers E2EE instances, and fetches the public measurement profiles and instance evidence. For each instance that has both evidence and an E2EE public key, it:

1. Decodes the TDX quote and computes `SHA256(nonce || e2e_pubkey)` using the provider's exact string concatenation format.
2. Requires the first 32 bytes of TDX `report_data` to equal that digest.
3. Rejects the TDX debug attribute.
4. Fetches DCAP collateral and verifies the quote with `dcap_qvl`.
5. Requires the verified quote measurements to match a published Chutes profile.
6. Records the granular TCB status returned by the quote verifier.
7. Sends available GPU evidence to NVIDIA NRAS, checks the returned overall result and nonce, and records the outcome as supplemental metadata.

An instance produces a binding only if steps 1 through 5 succeed. At least one instance binding is required for the provider result to be `verified`.

## Channel binding and forwarding

The binding contains the instance ID, the `chutes-ml-kem-768` algorithm label, and `SHA256(decoded public key bytes)`. The TDX report-data check proves that the evidence nonce and public key belong to the verified instance.

The backend selects only an instance present in the current verified binding set. It encapsulates to that ML-KEM-768 key, derives a ChaCha20-Poly1305 key, sends the encrypted body to `/e2e/invoke`, and decrypts the buffered or streaming response. A key digest mismatch or decryption failure rejects that attempt.

Each verified instance becomes its own attested session. Fleet membership changes do not change an unchanged instance's session ID.

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted from the verified TDX quote and bound E2EE channel. |
| `tcb_up_to_date` | Asserted only for `UpToDate`; another reported state is refuted; absence is unknown. |
| `gpu_attested` | Asserted as verifier-derived only when NRAS succeeds and its nonce matches. This proves a genuine CC GPU, not its binding to the serving CPU TEE. |
| `os_known_good` | Unknown. |
| `serving_software_known_good` | Unknown. |
| `model_weights_provenance` | Unknown. |

A stale TCB status is recorded, not rejected by the current bridge. Relying parties that require a current TCB must reject the refuted session claim.

## Limitations

- GPU evidence is supplemental. Failure or absence does not reject an otherwise verified CPU and E2EE-key binding.
- The bridge decodes the NRAS JWT returned over authenticated TLS and checks its signed-result fields and nonce, but does not independently verify that JWT against NRAS JWKS.
- Published measurement matching does not by itself prove model weights.
- Instance discovery and evidence endpoints are provider-controlled and rate-limited. The default three discovery rounds can increase cold-start cost.

## Reproduce

From the repository root, with `CHUTES_API_KEY` set:

```sh
request_hash="sha256:$(printf '0%.0s' {1..64})"
jq -n \
  --arg key "$CHUTES_API_KEY" \
  --arg hash "$request_hash" \
  '{
    api_version: "aci.provider-verifier.request.v1",
    provider: "chutes",
    upstream_name: "chutes-live",
    url_origin: "https://api.chutes.ai",
    model_id: "moonshotai/Kimi-K2.5-TEE",
    forwarded_body_hash: $hash,
    required: true,
    timeout_seconds: 300,
    provider_options: {
      chutes_api_key: $key,
      chutes_e2ee_discovery_rounds: "1"
    }
  }' | uv run python scripts/private_ai_provider_verifier.py
```

The command prints evidence and public bindings. Treat its output as sensitive operational evidence even though it should not echo the credential.
