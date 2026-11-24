# NEAR AI verification

The NEAR AI adapter verifies the public cloud gateway as a router-scoped TDX
channel. A successful buffered chat completion can also cite a response-bound
model enclave session when its signature matches verified preflight evidence.

| Property | Value |
| --- | --- |
| Attestation scope | Router, with supplemental response-bound model enclave sessions |
| Verifier | `scripts/provider_verifier/nearai.py` and vendored `NearAICloudVerifier` |
| External dependency | dstack verifier at `DSTACK_VERIFIER_URL` (default `http://localhost:8080`) |
| Enforced binding | `tls_spki_sha256` for forwarding, EIP-191 for supplemental receipt attribution |
| Evidence endpoint | `https://cloud-api.near.ai/v1/attestation/report` |

## Verification algorithm

The provider bridge asks NEAR AI for a report with a fresh nonce and
`include_tls_fingerprint=true`. It requires a non-empty `model_attestations[]`.
Every entry's `model_name` must match the configured upstream model ID exactly.
For a Chutes entry with neither `model_name` nor `intel_quote`, the bridge uses
the provider's `model` field for the same check. An alias that resolves to a
different name fails verification. An entry with the right name and no
`intel_quote` can establish the router session only.

The remote report fetch and NVIDIA verification use a 3-second connect
timeout and a 10-second read timeout. The Rust subprocess deadline also
bounds the local dstack verification, including any cold OS-image download.

`NearAICloudVerifier.verify_gateway_component` checks the
`gateway_attestation` component.

1. Send the TDX quote, event log, and VM configuration to the dstack verifier and require `is_valid: true`.
2. When both `app_compose` and `compose_hash` are present, require `SHA256(UTF8(app_compose))` to equal the reported hash.
3. Require a request nonce and signing address.
4. Parse the 64-byte `report_data` from the same quote bytes the dstack verifier accepted, and verify its layout:

   ```text
   report_data[0:32] = SHA256(signing_address || tls_cert_fingerprint)
   report_data[32:64] = request nonce
   ```

5. When an NVIDIA payload is present, require its nonce to match and require the NVIDIA verifier to succeed.

The bridge then requires a `gateway_attestation`, a `tls_cert_fingerprint`, and a valid component result, and emits one router-scoped TLS SPKI binding. A missing report-data value, nonce, signing address, or TLS fingerprint fails verification.

For response attribution, the bridge checks all reported model candidates
against the same fresh nonce. Each candidate needs a TDX quote, a replayed
event log, a measured compose hash matching the supplied compose, and quote
report data binding its ECDSA signing key and TLS fingerprint. A supplied GPU
payload must match the nonce and pass the NVIDIA check. The model evidence
bundle can contain up to eight candidates and 2 MiB of JSON. This supplemental
stage gets up to eight seconds within the verifier subprocess deadline.
Incomplete or invalid model evidence and a supplemental timeout leave the
verified router channel available. A key shared by several reported candidates
is ineligible for response attribution.

## Channel binding and forwarding

The accepted TDX quote covers the signing address, nonce, and TLS SPKI digest through `report_data`. The gateway pins that digest against the live NEAR AI HTTPS certificate before sending the request.

The verifier caches and refreshes verification by upstream model ID so each
model passes the name check. An explicit model name mismatch on refresh
immediately replaces even a current verified entry for that model because the
upstream now reports a different model. Requests reuse the mismatch for up to
60 seconds. If evidence is unavailable, the verifier keeps any still-valid
verified entry. Otherwise, it caches the failure for up to 10 seconds, capped
by the time between a scheduled refresh and the verified entry's expiry.
The receipt records the selected model, and the session records router
channel evidence.

Middleware injects a per-user `cache_salt`, honored by NEAR's TEE vLLM models. See [Prefix-cache isolation](../../prefix-cache-isolation.md).

## Buffered response binding

The adapter adds `x-no-aliasing: true` and `accept-encoding: identity` on the
buffered upstream chat path when a bearer token and pre-admitted model
candidate sessions are available. After a 2xx completion it requests
`GET /v1/signature/{chat_id}?signing_algo=ecdsa` and requires:

- A `provider_tee` ECDSA signature.
- The exact signed text `<canonical-model-id>:<request-hash>:<response-hash>`,
  using lowercase unprefixed SHA-256 hex over the bytes sent to and received
  from NEAR.
- An EIP-191 recovered signing address matching the advertised address and
  exactly one candidate from the verified report available at admission.

The receipt then cites the candidate session and records the signature in
`upstream.response_attested`. An unsuccessful lookup or binding records
`bound: false` when a lookup was eligible and leaves the response and router
session intact. A caller-pinned router session stays in use for the request.
Model sessions are available by the ID cited in a receipt. Router sessions
remain the discoverable pins because the gateway controls delivery to the
router before forwarding the prompt.

The lookup has a one-second end-to-end HTTP deadline, an 8 KiB body limit,
no retries or redirects, and at most 16 in-flight lookups per configured
upstream. A 429 pauses lookups for the advertised delay in seconds (capped at
60 seconds), or 10 seconds when that delay is unavailable. Busy, throttled,
and unavailable lookups leave the completed inference available.

Streaming, image generation, audio transcription, and other endpoints use
router TLS attestation and skip the signature lookup. A streaming receipt
commits to the observed bytes and attests the router channel. It carries no
response signature from the model enclave. NEAR image responses carry no
token usage, so middleware billing uses `images.count` and synthesizes
`usage.cost` from image pricing.

The round-1 live checks against `cloud-api.near.ai` rejected these tampered inputs:

| Tampered input | Rejection |
| --- | --- |
| Quote | `Dstack verification failed: Quote verification failed` |
| Request nonce | `Report data check failed: mismatch` |
| `tls_cert_fingerprint` | `Report data check failed: mismatch` |

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted as hardware-proven from the verified TDX quote and bound TLS channel. |
| `tcb_up_to_date` | Hardware-proven from the dstack verifier's TCB status: asserted for `UpToDate`, refuted for any other reported status, unknown when absent. |
| `gpu_attested` | Unknown. Supplemental GPU-check presence and acceptance are preserved in `claims.extra.gateway_gpu_check`, without an authenticated device-identity or CPU-binding claim. |
| `os_known_good` | Unknown. |
| `serving_software_known_good` | Unknown. |
| `model_weights_provenance` | Unknown. |

For a response-bound model session, the TCB claim reflects both the gateway
and the selected candidate. The OS claim requires verified production OS
images on both, and a verified development image on either refutes the claim.
The model's NVIDIA check is supplemental verifier-derived evidence. The
individual gateway and model TCB statuses are available in `claims.extra`.

## Limitations

- No workload pin. Any dstack CVM that passes the dstack verifier with matching `report_data` is accepted as the NEAR AI gateway. The adapter does not compare the compose, app ID, or image with a reviewed value.
- The canonical model name in the attestation response is report metadata.
  For bound chat completions, the model name and hashes are also in the
  signed response statement.
- Configure canonical upstream IDs from `/v1/model/list`. NEAR resolves
  admin-controlled aliases on the server. See the
  [round-2 findings](review.md#open-findings).
- A bound signature proves that a holder of the reported model key signed the
  exact exchange. NEAR can share a signing key across replicas and measured
  deployments. Uniqueness in an observed report does not prove the identity
  of a physical replica or rule out other key holders. See
  [NEAR's verification policy](https://docs.near.ai/cloud/verification/reference/verification-policy)
  and [nearai/cloud-api#1422](https://github.com/nearai/cloud-api/issues/1422).
- A non-`UpToDate` TCB status can remain `is_valid` and is represented as a refuted typed claim, not a bridge failure.
- Gateway verification accepts missing compose material. Model response
  attribution requires the measured compose and its hash. Workload and model
  weights allowlisting remain outside this verification.
- Supplemental GPU verification can run when the gateway report includes a payload. The bridge preserves its presence and acceptance in `gateway_gpu_check`, and reviewed material includes those outcomes. The NVIDIA path decodes JWTs without signature verification and does not bind device identity to the serving CPU, so it cannot establish authenticated GPU identity or continuity.

## Tests and reproduction

Run the hermetic report-data, model selection and bridge checks.

```sh
cargo test --locked --test soundness_report_data --test session_material_soundness
CARGO_BUILD_JOBS=4 cargo test --locked --lib near_ai_caches_model_failures_and_refreshes_healthy_siblings
CARGO_BUILD_JOBS=4 cargo test --locked --lib aci::upstream::near
```

For a live verifier run, start a trusted dstack verifier and set
`DSTACK_VERIFIER_URL` explicitly. The
[2026-11-23 PST live check](review.md#live-check-2026-11-23-20072012-pst) used
dstack-verifier 0.6.1 and the attestation endpoint answered without credentials.
The bridge sends a bearer token when one is configured.

```sh
export DSTACK_VERIFIER_URL='http://localhost:18080'
python3 - <<'PY' | uv run python scripts/private_ai_provider_verifier.py
import json

print(json.dumps({
    "api_version": "aci.provider-verifier.request.v1",
    "provider": "near-ai",
    "upstream_name": "near-ai-live",
    "url_origin": "https://cloud-api.near.ai",
    "model_id": "Qwen/Qwen3.8-27B",
    "forwarded_body_hash": "sha256:" + "0" * 64,
    "required": True,
    "timeout_seconds": 300,
}))
PY
```
