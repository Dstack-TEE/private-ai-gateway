# ArmetAI Verification

The `armet-ai` adapter verifies one per-model Intel TDX endpoint that does
**not** run on dstack. There is no event log to replay and no dstack verifier:
the workload identity is the measured TD itself (MRTD, RTMR0 to RTMR3 and its
launch configuration), pinned by the operator. The channel is the TLS key that
TD commits to in its quote, and the TD's NVIDIA confidential-computing GPUs
must pass NRAS attestation for the same nonce.

| Property | Value |
| --- | --- |
| Provider configuration | `"provider": "armet-ai"` |
| Attestation scope | Per model |
| Verifier | `scripts/provider_verifier/armet_ai.py` through the provider-verifier bridge |
| Verifier ID | `private-ai-verifier/armet-ai/v1` |
| CPU evidence | Intel TDX quote verified with `dcap-qvl` (`get_collateral_and_verify`) |
| GPU evidence | NVIDIA evidence verified with NRAS through `secretvm-verify` (signed result); mandatory |
| Workload identity | Operator pin over the verified MRTD, RTMR0-3, TD_ATTRIBUTES, XFAM, MRCONFIGID, MROWNER and MROWNERCONFIG |
| Enforced binding | `tls_spki_sha256` |
| Evidence endpoint | `<base_url>/v1/attestation/report?nonce=<64 hex>&model=<provider model id>&version=2` |

## Producer requirement

The provider serves each model from its own TD at its own HTTPS origin and
implements the report endpoint above. For every request it must:

1. Take `nonce` (32 bytes, hex) and `model` from the query string.
2. Refuse unless `model` is exactly the model this TD serves. The gateway sends
   the provider model ID, the value side of the upstream `models` map.
3. Compute `spki_sha256`, the SHA-256 of the DER SubjectPublicKeyInfo of the
   TLS leaf certificate this TD serves on its origin.
4. Collect NVIDIA GPU evidence for every GPU attached to the TD, using `nonce`
   as the GPU attestation nonce. Serialize it once as the JSON text
   `nvidia_payload`, an object with `arch`, `nonce` and `evidence_list` in the
   format NVIDIA's attestation SDK produces. The exact UTF-8 bytes matter: the
   TD commits to their hash and the gateway hashes the string it receives.
5. Request a TD quote with this 64-byte `report_data`:

   ```text
   digest            = SHA256("private-ai-gateway/armet-ai/v2" || 0x00
                              || nonce (32 bytes)
                              || spki_sha256 (32 bytes)
                              || SHA256(UTF8(nvidia_payload)) (32 bytes)
                              || UTF8(model))
   report_data[0:64] = ASCII(lowercase hex(digest))     # 64 characters
   ```

   The field holds the 64 lowercase hex characters of the digest, not the raw
   32-byte digest. Raw-digest or uppercase encodings are rejected.

6. Return `200` with `Content-Type: application/json`:

   ```json
   {
     "version": 2,
     "model_id": "<model>",
     "nonce": "<nonce, echoed>",
     "tls_spki_sha256": "<64 lowercase hex>",
     "quote": "<hex TDX v4 or v5 quote>",
     "nvidia_payload": "{\"arch\":\"HOPPER\",\"nonce\":\"<nonce>\",\"evidence_list\":[...]}"
   }
   ```

   `nvidia_payload` is a JSON **string**, not a nested object.

A reference computation of the report data, for the provider's attestation
service:

```python
import hashlib
from cryptography import x509
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

def armet_ai_report_data(nonce_hex: str, model: str, cert_pem: bytes,
                         nvidia_payload: str) -> bytes:
    cert = x509.load_pem_x509_certificate(cert_pem)
    spki = cert.public_key().public_bytes(Encoding.DER, PublicFormat.SubjectPublicKeyInfo)
    digest = hashlib.sha256(
        b"private-ai-gateway/armet-ai/v2\x00"
        + bytes.fromhex(nonce_hex)
        + hashlib.sha256(spki).digest()
        + hashlib.sha256(nvidia_payload.encode()).digest()
        + model.encode()
    ).hexdigest()
    return digest.encode("ascii")   # 64 bytes: the configfs-tsm inblob / TDREPORT report_data
```

The TLS binding is meaningful only when the TLS private key is generated inside
the TD, never leaves it, and TLS terminates inside the TD. A load balancer, CDN
or ingress that terminates TLS outside the TD breaks the custody claim even
though the digest appears in the quote.

## Required configuration

```json
{
  "name": "armet-llama-70b",
  "provider": "armet-ai",
  "base_url": "https://llama-70b.armetai.example",
  "models": { "armet/llama-3.3-70b": "llama-3.3-70b-instruct" },
  "accepted_subjects": [
    "tdx-measurement:sha256:<64 lowercase hex>"
  ],
  "bearer_token": "optional, sent to the report endpoint and on inference"
}
```

The gateway refuses to load an `armet-ai` entry without at least one
well-formed `accepted_subjects` pin, or with a `base_url` that is not a root
HTTPS origin without user info, path, query or fragment.

A pin is SHA-256 over these raw fields of the verified TD report, in order:

```text
tdx-measurement:sha256:<hex(SHA256(MRTD || RTMR0 || RTMR1 || RTMR2 || RTMR3
                                   || TD_ATTRIBUTES || XFAM
                                   || MRCONFIGID || MROWNER || MROWNERCONFIG))>
```

The five measurement registers identify the software. The other fields pin how
the host launched it, so the same image launched with different TD attributes
(for example without `SEPT_VE_DISABLE`), different XSAVE features, or a
non-default config or owner ID does not match. ArmetAI sets `MRCONFIGID` to the
hash of the TD's `initdata.toml`, so a change to that launch configuration also
changes the pin.

To obtain a candidate pin, run the bridge against the endpoint with a
placeholder pin (see [Tests and reproduction](#tests-and-reproduction)). The
failure names the observed subject and returns every pinned field as
`observed_measurements`. Review the release before adding it: a pin is an
acceptance decision, not a formality.

What each register covers depends on the provider's boot chain, and the
provider must document it. With TDVF firmware and a measured direct-boot
kernel, MRTD covers the firmware, RTMR0 the firmware configuration and ACPI
tables, RTMR1 the kernel and bootloader, and RTMR2 the kernel command line and
initrd. RTMR3 is for the provider to extend. Because RTMR0 includes the ACPI
tables, a different vCPU or memory shape produces a different pin; list one pin
per VM shape. Every value must be stable across restarts of the same release
and shape, or the pin will not match.

## Verification algorithm

For an uncached verification, the adapter:

1. Refuses if no pin is configured, or the origin is not HTTPS.
2. Generates a fresh 32-byte nonce and fetches the report with the configured
   bearer token, if any.
3. Requires a JSON object with `version` 2, the echoed nonce, `model_id` equal
   to the requested provider model ID, a 64-hex `tls_spki_sha256`, a quote, and
   an `nvidia_payload` string of at most 2 MiB that parses to an object with a
   non-empty `evidence_list` and `nonce` equal to the request nonce.
4. Rejects a debug TD: the TUD byte of `TD_ATTRIBUTES` (quote offset 168) must
   be zero.
5. Verifies the quote to the Intel root with `dcap-qvl`, including collateral.
   Any verification error rejects.
6. Rejects a `Revoked` platform TCB. Other statuses are recorded (see
   [Limitations](#limitations)).
7. Reads `report_data` and the pinned fields from the **verified** report, not
   from the raw quote bytes, and requires `report_data` to equal the
   commitment in [Producer requirement](#producer-requirement), computed over
   the exact `nvidia_payload` string received.
8. Requires the measurement subject to be in `accepted_subjects`.
9. Verifies `nvidia_payload` with NRAS through
   `secretvm.verify.check_nvidia_gpu_attestation`, and gates on the signed
   result: it must be valid, its overall result `true`, its signed nonce equal
   to the request nonce, and every per-GPU report must verify that nonce.
10. Emits the attested `tls_spki_sha256` as the channel binding, with
    `attested_scope: "model"`.

Every step gates. GPU evidence that was not committed to by the verified quote
fails at step 7 and is never sent to NRAS.

## Channel binding and forwarding

The verified quote commits to the nonce, the TLS SPKI, the GPU evidence and the
model ID in one digest. A replayed quote, a quote for a different key, a quote
from a TD serving another model, and GPU evidence swapped in from elsewhere all
fail step 7. The backend's `SpkiPinVerifier` compares the bound SPKI digest
with the live HTTPS certificate before forwarding. A certificate rotation
therefore needs a fresh verification, which the gateway performs on the next
lease.

The Rust scope seam refuses a bridge result that declares `router` scope for
this provider, so one model's verification cannot be reused for another model
behind the same origin.

## Evidence

`evidence.data` is the exact response body the checks consumed, with its
content type, and `evidence.digest` is `sha256:` over those bytes. It is not
re-serialized. An auditor can re-run steps 3 to 9 from it offline, with network
access for collateral and NRAS.

## Provider claims recorded

| Key | Value |
| --- | --- |
| `trust_boundary` | `armet-ai-td` |
| `evidence_scope` | `model_instance` |
| `canonical_model_id` | The provider model ID. |
| `report_version` | `2` |
| `report_data_nonce_matched` | `true` |
| `tls_spki_from_report_data` | `true` |
| `model_id_from_report_data` | `true` |
| `gpu_evidence_from_report_data` | `true` |
| `tdx_debug_mode` | `false`, because debug TDs are rejected. |
| `accepted_subject` | The matched pin. |
| `measurements` | Every pinned field from the verified report, as hex. |
| `tdx_module` | `mr_seam` and `tee_tcb_svn` of the TDX module, as hex. Recorded, not pinned. |
| `tcb_status` | `dcap-qvl` TCB status, such as `UpToDate`. |
| `advisory_ids` | Intel advisory IDs reported with that status. |
| `gpu_verified` | `true`; a result without it is never emitted. |
| `gpu_models` | GPU models from the NRAS-signed per-GPU reports. |
| `gpu_count` | Number of NRAS-signed per-GPU reports. |

## Session claims

| Claim | Mapping |
| --- | --- |
| `tee_attested` | Asserted, hardware-proven, from the verified quote and bound channel. |
| `tcb_up_to_date` | Hardware-proven tri-state from `tcb_status`: asserted for `UpToDate`, refuted otherwise, unknown when absent. |
| `serving_software_known_good` | Asserted, verifier-derived, naming the matched operator pin. |
| `os_known_good` | Unknown. The pin covers firmware and kernel, but it is an operator's acceptance of a measurement, not a reproducible build. |
| `gpu_attested` | Asserted, verifier-derived, from the signed NRAS result, citing the signed GPU models and the report_data commitment. |
| `model_weights_provenance` | Unknown. |

## Limitations

- **Provenance is operator trust.** A pin proves which measured TD is running,
  not that its code maps to reviewed source. Strict inclusion (audit criteria
  7 and 13) needs a reproducible build or signed provenance for the firmware,
  kernel, initrd and application, and published measurements before rollout.
- **TCB freshness is recorded, not gated,** except `Revoked`. This matches the
  other TDX adapters. A relying party that needs `UpToDate` must apply a claims
  policy on `tcb_up_to_date`.
- **Model binding is a claim of the measured software.** `report_data` commits
  to the model ID the TD says it serves. Whether the weights match that ID is
  only as strong as the measured software's loading logic.
- **Key custody is not proved by the quote alone.** Reviewers must confirm the
  TLS key is generated and held inside the TD, and that nothing outside the TD
  terminates TLS.
- **GPU-to-TD binding rests on the measured software.** The quote proves the
  pinned TD software presented this GPU evidence for this nonce. That the GPUs
  are the ones attached to the TD, in CC mode, and serving the requests, is a
  property of that software and the NVIDIA driver, not of the TDX quote.
- **Every release changes the pin.** A new firmware, kernel, image or VM shape
  changes a pinned field, and the gateway fails closed until the new subject
  is pinned.

## Tests and reproduction

Run the hermetic bridge test:

```sh
cargo test --test armet_ai_bridge
```

It runs `tests/provider_verifier/armet_ai_soundness.py`. The script replaces
the HTTP fetch, `dcap-qvl` and NRAS with fixtures that react to the bridge's
real nonce, and keeps pinning, the `report_data` commitment, the GPU gate,
debug and TCB policy, evidence emission and scope real.

| Input | Outcome |
| --- | --- |
| Genuine report, pinned measurement, NRAS-valid GPUs | Verified with the `tls_spki_sha256` binding, model scope, exact-bytes evidence and signed GPU claims. |
| TDX 1.5 (`TD15`) verified report | Verified. |
| `OutOfDate` TCB | Verified with `tcb_status: OutOfDate`. |
| `Revoked` TCB | Rejected. |
| No configured pin | Rejected. |
| Plain-HTTP origin | Rejected. |
| Unreachable endpoint or non-JSON body | Rejected. |
| Version 1 report, wrong echoed nonce, or `model_id` | Rejected. |
| Malformed `tls_spki_sha256` | Rejected. |
| Debug TD | Rejected. |
| `dcap-qvl` verification error | Rejected. |
| Quote bound to an old nonce, another TLS key, or another model | Rejected as a `report_data` failure. |
| Report advertising a different TLS key than the quote binds | Rejected as a `report_data` failure. |
| Correct digest in raw-bytes or uppercase-hex encoding | Rejected as a `report_data` failure. |
| GPU evidence substituted after quoting | Rejected as a `report_data` failure, without an NRAS call. |
| Any one pinned field changed | Rejected; the failure reports the observed fields. |
| GPU evidence missing, not a string, not JSON, empty, or with a stale nonce | Rejected. |
| NRAS invalid, erroring, overall `false`, wrong signed nonce, one GPU failing the nonce, or no per-GPU reports | Rejected. |

Rust coverage: `src/aggregator/upstream_config/tests.rs` (pin and origin
validation, per-model scope), `src/aci/verifier/tests.rs` (bridge seam, option
passing, refusal of a declared `router` scope), and
`src/aggregator/service/claims.rs` (claim mapping).

For a live endpoint:

```sh
request_hash="sha256:$(printf '0%.0s' {1..64})"
jq -n \
  --arg origin 'https://llama-70b.armetai.example' \
  --arg model 'llama-3.3-70b-instruct' \
  --arg pin 'tdx-measurement:sha256:0000000000000000000000000000000000000000000000000000000000000000' \
  --arg hash "$request_hash" \
  '{
    api_version: "aci.provider-verifier.request.v1",
    provider: "armet-ai",
    upstream_name: "armet-ai-live",
    url_origin: $origin,
    model_id: $model,
    forwarded_body_hash: $hash,
    required: true,
    timeout_seconds: 60,
    provider_options: {("armet_ai_accepted_subject:" + $pin): "true"}
  }' | uv run python scripts/private_ai_provider_verifier.py
```

With the placeholder pin, a correct endpoint fails only at the measurement step
and prints the subject to review. Replace the placeholder with that subject
after review, and the same command returns `verified`.

For the live E2E suite, add a providers-file entry with `"provider":
"armet-ai"`, `"binding": "tls_spki_sha256"` and the reviewed pin in
`accepted_subjects` (see [Live E2E test suite](../../live-e2e-test-suite.md)).

See the [dated review](review.md) for the audit-criteria self-assessment.
