# Chutes verification

The Chutes adapter verifies each discovered TDX instance and encrypts provider traffic to that instance's attested ML-KEM public key.

| Property | Value |
| --- | --- |
| Attestation scope | Per instance |
| Verifier | `scripts/provider_verifier/chutes.py` through the provider-verifier bridge |
| Verifier ID | `private-ai-verifier/chutes/v1` |
| Enforced binding | `e2ee_public_key_sha256` |
| Transport | ML-KEM-768, HKDF-SHA256, and ChaCha20-Poly1305 over `/e2e/invoke` |

See [Private Chutes configuration](configuration.md) for dedicated origins and chute-ID pins.

## Verification algorithm

The adapter resolves the model's chute ID and fetches three documents from the Chutes API:

- the accepted measurement profiles from `/servers/tdx/measurements`, with
  `/servers/tee/measurements` as a fallback for deployments that lack the TDX route;
- nonce-bound instance evidence from `/chutes/{chute_id}/evidence?nonce=<random>`; and
- the E2EE instances and their public keys from `/e2e/instances/{chute_id}`.

The adapter compares `tee_type` case-insensitively and skips evidence tagged
with another type. Evidence with a missing or null tag must pass the same TDX
checks. For each eligible instance with evidence and an E2EE public key, the
adapter runs these checks.

1. Computes `expected = SHA256(nonce || e2e_pubkey)` over the provider's exact string concatenation.
2. Reads `report_data` from the TDX quote at byte offset `48 + 520` and requires its first 32 bytes to equal `expected`. Chutes uses the upper 32 bytes for a hash of the attestation proxy certificate's public key. The adapter checks only the first 32 bytes for E2EE binding. [Chutes quote verification source](https://github.com/chutesai/chutes-api/blob/3b5609f4/api/server/util.py#L1235-L1265)
3. Rejects the TDX debug attribute.
4. Fetches DCAP collateral and verifies the quote with `dcap_qvl`.
5. Requires the verified MRTD and RTMR0-3 to match one of the fetched measurement profiles.
6. Records the TCB status returned by the quote verifier.
7. Posts the GPU evidence to NVIDIA NRAS with `expected` as the nonce. It verifies the platform JWT's ES384 signature against NVIDIA's published JWKS and checks the issuer, platform subject, and expiry. It records a positive supplemental result only when the signed `x-nvidia-overall-att-result` is true and `eat_nonce` equals `expected`. [NVIDIA NRAS JWKS](https://docs.api.nvidia.com/attestation/reference/nras-jwks-api)

An instance produces a binding only if steps 1 through 5 succeed. At least one instance binding is required for the provider result to be `verified`. A wrong nonce or a substituted E2EE key fails step 2 with `Chutes E2EE key binding does not match report_data`, and a tampered quote fails DCAP signature verification.

## Channel binding and forwarding

The binding contains the instance ID, the `chutes-ml-kem-768` algorithm label, and `SHA256(decoded public key bytes)`. The TDX report-data check proves that the evidence nonce and public key belong to the verified instance.

The backend selects only an instance present in the current verified binding set, using a single-use nonce issued for that instance. [Chutes provider sessions](../../upstream-verification-lifecycle.md#chutes-provider-sessions) describes the nonce pool and its refresh.

Each verified instance becomes its own attested session. Fleet membership changes do not change an unchanged instance's session ID.

## Encrypted transport

The gateway encrypts the whole provider request to the selected instance and sends it to `<chutes_e2ee_api_base>/e2e/invoke`, which defaults to `https://api.chutes.ai`. The TLS connection to that origin is ordinary CA-validated TLS. Confidentiality comes from the encryption, not from a TLS pin. The implementation is in `src/aci/upstream/chutes/crypto.rs`.

For the request, the gateway:

1. Generates a fresh ML-KEM-768 response key pair and adds its public key to the JSON body as `e2e_response_pk`.
2. Encapsulates to the instance's attested ML-KEM-768 public key.
3. Derives a 32-byte key with HKDF-SHA256, using the shared secret as input key material, the first 16 bytes of the ML-KEM ciphertext as salt, and `e2e-req-v1` as info.
4. Gzips the JSON body and encrypts it with ChaCha20-Poly1305 under a random 12-byte nonce.
5. Posts `ML-KEM ciphertext || nonce || ciphertext` as `application/octet-stream`, with `X-Chute-Id`, `X-Instance-Id`, `X-E2E-Nonce`, `X-E2E-Stream`, and `X-E2E-Path` headers.

Only the gateway holds the response private key, and it keeps a separate one for each request. The instance encrypts its answer to the response public key:

- **Buffered response.** The body has the same `ML-KEM ciphertext || nonce || ciphertext` layout. The gateway decapsulates with the response private key, derives the key with info `e2e-resp-v1`, decrypts, and gunzips.
- **Streaming response.** An `e2e_init` SSE event carries an ML-KEM ciphertext. The gateway decapsulates it and derives one stream key with info `e2e-stream-v1`. Each later `e2e` event carries `nonce || ciphertext`, which decrypts to one plaintext SSE event. A chunk before `e2e_init` is rejected. `usage` data text passes through, and an `e2e_error` event becomes an `error` event. The outer SSE envelope uses the shared `sse-stream` decoder with a 16 MiB block cap. Generated application events use the library encoder; incomplete outer blocks are discarded, and invalid UTF-8 fails the stream. These framing rules do not bypass ML-KEM initialization or chunk authentication.

The gateway decrypts before receipt finalization, so the receipt's response hash covers the plaintext returned to the client. A key digest mismatch, a malformed blob, or an authentication failure rejects that attempt.

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted as hardware-proven from the verified TDX quote and bound E2EE channel. |
| `tcb_up_to_date` | Hardware-proven from the instance's TCB status: asserted for `UpToDate`, refuted for any other reported status, unknown when absent. |
| `gpu_attested` | Asserted as verifier-derived when this instance has a valid, positive NRAS platform JWT with a matching nonce. Unknown otherwise. |
| `os_known_good` | Unknown. The verifier does not classify the OS image. |
| `serving_software_known_good` | Unknown. |
| `model_weights_provenance` | Unknown. |

The bridge records a stale TCB status instead of rejecting it. Relying parties that require a current TCB must reject the refuted session claim.

## Limitations

- Middleware injects a per-user, provider-separated `cache_salt` into text-generation bodies before receipt hashing and Chutes encryption. Caller salt strings only subdivide the user's partition. Passthrough to vLLM is an observed provider behavior, not an attestation-backed cache-isolation guarantee. See the [live isolation observation](../../prefix-cache-isolation.md#observed-enforcement) and [disposable-CVM runbook](../../cache-salt-cvm-test.md). Hermetic coverage: `cargo test --test middleware_completion cache_salt`.
- The adapter uses Chutes' public measurement feed as its allowlist. It falls back to the deprecated feed on HTTP 404, 405, 410, or 501. Both feeds returned the same measurement tuples in [round 2](review.md#round-2-2026-11-23-pst). The public Chutes API source omits release-candidate profiles from the legacy feed, including those for debug builds. [Chutes measurement source](https://github.com/chutesai/chutes-api/blob/3b5609f4/api/server/router.py#L485-L522), [sek8s measurement guide](https://github.com/chutesai/sek8s/blob/v1.5.0/docs/reproducing-measurements.md)
- GPU evidence is supplemental. When GPU evidence is available after the CPU
  and E2EE-key checks, the adapter fetches the JWKS once with a five-second
  limit and shares it across the instance checks. The whole GPU step gets up
  to ten seconds and stops sooner if needed to leave two seconds for the
  verifier to exit. Failed, missing, and timed-out GPU results leave the GPU
  claim unknown. A verified CPU and E2EE-key binding remains eligible.
- GPU evidence is not hardware-bound to the serving CPU TEE. A passing NRAS result shows a genuine confidential-computing GPU for the nonce, not that it served the request.
- Measurement matching does not prove which model weights are loaded.
- Instance discovery and evidence endpoints are provider-controlled and rate-limited. `/e2e/instances` limits aggressively: the default three discovery rounds can trigger a `429` on a cold chute, and `chutes_e2ee_discovery_rounds: "1"` is gentler.

## Reproduce

Run the hermetic checks with `uv run --locked python tests/provider_verifier/session_material_soundness.py`.

Set `PROVIDER_API_KEY` to your Chutes API key, then run this command in Bash
from the repository root.

```sh
request_hash="sha256:$(printf '0%.0s' {1..64})"
jq -n \
  --arg key "$PROVIDER_API_KEY" \
  --arg hash "$request_hash" \
  '{
    api_version: "aci.provider-verifier.request.v1",
    provider: "chutes",
    upstream_name: "chutes-live",
    url_origin: "https://api.chutes.ai",
    model_id: "moonshotai/Kimi-K3-TEE",
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
