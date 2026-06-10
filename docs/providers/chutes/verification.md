# Chutes — attested session verification & binding

- **TEE:** Intel TDX (CPU) + NVIDIA Confidential Compute (GPU)
- **Session binding:** `e2ee_public_key_sha256`
- **Verifier:** the provider-verifier bridge, directly (`scripts/private_ai_provider_verifier.py`
  → `verify_chutes` / `chutes_verify_instance`). Uses `dcap_qvl` for the quote; no
  vendored verifier class.
- **Status:** sound.
- **Audit:** see [review.md](review.md).

## What is verified

For each E2EE instance Chutes returns, `chutes_verify_instance` does, in order:

1. **Fetch evidence.** Pull the instance's TDX quote and GPU evidence from the Chutes
   API (`/e2e/instances/{chute_id}`, `/chutes/{chute_id}/evidence`).
2. **Bind the E2EE key into the quote.** Compute
   `expected = SHA256(nonce ‖ e2e_pubkey)`, parse `report_data` from the quote bytes
   (`chutes_report_data`, TDX v4 offset `quote[48+520 : 48+584]`), and require
   `report_data[0:32] == expected`. This is the anti-tamper binding.
3. **Reject debug mode.** `chutes_debug_enabled` checks the TD attributes debug bit.
4. **Verify the quote.** `dcap_qvl.get_collateral_and_verify(quote)` performs real Intel
   DCAP verification (fetches collateral, checks the signature chain). The status must
   be `UpToDate`.
5. **Match the measurement profile.** The quote measurements must match a reviewed
   public profile (`chutes_measurement_name` against the provider reference).
6. **Verify the GPU.** `chutes_verify_gpu` POSTs the GPU evidence to NVIDIA NRAS over
   TLS with `nonce = expected_report_data`, requires `x-nvidia-overall-att-result`, and
   checks `eat_nonce == expected_report_data`.

## What binds the session

The E2EE public key is bound into the TDX quote's `report_data`
(`report_data[0:32] = SHA256(nonce ‖ e2e_pubkey)`), and the quote signature is verified
by DCAP. So possession of a decryptable channel under that key implies you are talking
to the attested enclave. The emitted binding is
`e2ee_public_key_sha256 = SHA256(decoded ML-KEM public key)`.

## What a tamper rejects

- Tampered quote → DCAP signature verification fails (confirmed live:
  `ISV enclave report signature is invalid`).
- Wrong nonce → `report_data[0:32] != SHA256(nonce ‖ e2e_pubkey)` →
  `Chutes E2EE key binding does not match report_data`.
- Wrong/forged E2EE key → same binding mismatch.
- `OutOfDate`/`SWHardeningNeeded` TCB → rejected (only `UpToDate` accepted).

## Transport enforcement

The backend encrypts each request body to the verified E2EE public key
(ML-KEM-768 + HKDF-SHA256 + ChaCha20-Poly1305) and sends it to `/e2e/invoke`, then
decrypts the response. A response that decrypts proves the bound enclave served it.

## Notes

- Cold evidence verification is slow (~138 s); it runs off the request path via the
  verification lease + a pooled nonce session. See the lifecycle doc.
- `/e2e/instances` is aggressively rate-limited; the default
  `chutes_e2ee_discovery_rounds: 3` can self-trigger a `429` on a cold chute.
  `rounds: 1` is gentler.
- NVIDIA NRAS tokens are fetched online over TLS and nonce-checked; the JWT signature
  itself is not additionally verified against NRAS' JWKS (tracked defense-in-depth
  follow-up in the roadmap).

## Reproduce

```bash
set -a; . /home/h4x/workspace/redpill/.env; set +a
cd /home/h4x/workspace/redpill/private-ai-gateway
echo '{"api_version":"aci.provider-verifier.request.v1","provider":"chutes",
  "upstream_name":"chutes-live","url_origin":"https://api.chutes.ai",
  "model_id":"moonshotai/Kimi-K2.5-TEE",
  "forwarded_body_hash":"sha256:'"$(printf '0%.0s' {1..64})"'","required":true,
  "timeout_seconds":300,
  "provider_options":{"chutes_api_key":"'"$CHUTES_API_KEY"'","chutes_e2ee_discovery_rounds":"1"}}' \
  | uv run python scripts/private_ai_provider_verifier.py
```
