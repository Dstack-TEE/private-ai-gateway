# ArmetAI per-model TDX review

Date: 2026-11-23 PST (currency check).

> [!NOTE]
> This is a dated admission record. See [Documentation conventions](../../README.md#documentation-conventions).
> The [currency check](#currency-check-2026-11-23-pst) records the maintainers'
> findings. The [historical self-assessment](#historical-self-assessment)
> covers the earlier vLLM deployment.

ArmetAI operates `https://ctf.armet.ai` on a Fortanix confidential-computing
stack. The earlier `poc1.armet.ai` endpoint did not resolve during this check.

## Currency check (2026-11-23 PST)

The review covered Fortanix's attestation reference at `5ba767a9`, dated
2026-11-19 PST, and the public workload image it pins by manifest digest
`sha256:b5269d74e1506add71a61566ad9a94f8e1b41fa7ef4170ba3f2cde176dd0b1c4`.
The image contains SGLang `v0.5.23` at `297e54fd` and ArmetAI's entrypoint,
attestation server, TLS key generation and ACME scripts under `/opt/cvn`.
Fortanix identifies guest components at `d3ae8f2c` as part of the guest measured
in RTMR1 and RTMR2.
[Attestation reference](https://github.com/fortanix/armet-ctf/tree/5ba767a9553173bb4fc3977a979f8e7cbc0e36b8),
[SGLang source](https://github.com/sgl-project/sglang/commit/297e54fd105e974cdf401e5b4d20253a22c16f90),
[Guest components source](https://github.com/fortanix/guest-components/commit/d3ae8f2c572e474d7fd36b76ba4f10042c05fefd).

### Current verdict

**Decision pending.** The report endpoint now requires an API key. The
unauthenticated probe yielded no fresh quote, so the gateway's verification
and channel binding could not be confirmed for the SGLang deployment.

### Changes since the October record

| Item | 2026-10-15 record | November public reference or observation |
| --- | --- | --- |
| Source and workload image | Source review pending, image digest unrecorded | Fortanix reference at `5ba767a9` and digest-pinned image above |
| Serving engine | vLLM | SGLang `v0.5.23` |
| MRCONFIGID | `bc080317…d039aa` | `e5cdf268…5225` (four changes since the October record) |
| RTMR0 | `484e310a…db14` in the October provider reference | `b5b6a409…5419` |
| MRTD, RTMR1, RTMR2, RTMR3 | October provider values | Same values in the November reference |
| Measurement subject | `da6f26e8…0c8a`, quote-verified | **Predicted** `1e2dcdb9…b9ba` |
| TLS leaf and SPKI | Let's Encrypt `YE2`, `a2e07c25…bb2c` | Let's Encrypt `YE1`, `caf91a40…fed8` observed |
| Report authentication | Unauthenticated requests worked | Provider documents a bearer API key |
| GPU topology | 8 × GH100 verified | 8 × H200 SXM and 4 × NVSwitch in one NVIDIA Protected PCIe group, per provider |
| Weights | Provenance unknown | Initdata pins a SHA-384 digest and the provider publishes a per-file manifest |

The **predicted subject** is
`tdx-measurement:sha256:1e2dcdb9709a6a399233e4bb78dd3cea9de4dfde74d9a5e5439881d33977b9ba`.
This calculation uses the published registers, TD_ATTRIBUTES
`0000001000000000`, XFAM `e702060000000000` and zero MROWNER and
MROWNERCONFIG as in the October record. The same calculation reproduces the
October quote-verified subject from the October reference at `a0599a97`.
Treat the November result as a candidate for the authenticated probe.

### Live and public checks (2026-11-23 20:07–20:09 PST)

- `ctf.armet.ai` resolved to `52.53.179.221`. The observed leaf
  (`CN=ctf.armet.ai`) was valid from 2026-11-19 00:08 PST and had
  SPKI SHA-256 `caf91a407e44b55e00aabc69c6990599e5e3e624619de887981b9b995cfbfed8`.
- A v2 report request for `GLM-5.3-Flash` with a fresh nonce and no credential
  returned HAProxy HTML with HTTP `429` on three attempts. An unauthenticated
  request for the wrong model also returned `429`. These responses did not
  establish whether the `429` was due to rate limiting or missing credentials.
  `/` and `/v1/models` returned `401`, and `/healthz` returned `404`.
- SHA-384 of `initdata.toml` at `5ba767a9` matched the published MRCONFIGID
  `e5cdf268…5225`. At `a0599a97` it matched the October quote value
  `bc080317…d039aa`.
- The Docker Hub manifest bytes matched the pinned image digest. SHA-384 of
  `weights-manifest.txt` matched `MODEL_WEIGHTS_SHA384` in `initdata.toml`
  (`b52f7ca1…35a5`).

### Findings from the published configuration and image

- **Workload and TLS custody (criteria 1 and 2).** The public initdata embeds
  a policy that requires exact environment, mounts and digest-pinned workload
  and pause images, and denies exec, policy replacement, stdin writes, RNG
  reseeding, clock setting and tracing. The reviewed image generates the TLS
  key at boot in guest tmpfs for the in-TD HAProxy user. The ACME helper uses
  TLS-ALPN-01 as a separate user and renews with the same CSR. The attestation
  service computes the adapter's v2 `report_data` commitment and reads
  the model ID from the engine.
- **Privacy boundary (criterion 5).** The policy allows `ReadStreamRequest`,
  so the host can read container output. The TLS-terminating `haproxy.cfg`
  and API-key files sit in encrypted image layers. Initdata configures Fortanix
  DSM for layer-key release. The proxy's routing and logging settings require
  provider disclosure.
- **GPU evidence (criterion 6).** The image's report service adds
  `switch_arch`, `switch_evidence_list` and `switch_evidence_present` to
  `nvidia_payload`. The adapter passes the full payload to NRAS for GPU
  verification and has no switch verification step. NRAS acceptance of the
  expanded payload remains untested.
- **Release publication (criterion 7).** The image build, TLS leaf validity
  start and reference commit fall within about an hour on 2026-11-19 PST.
  The exact rollout time remains unconfirmed. The public record provides
  no evidence of advance publication, release tags, a changelog or an
  emergency publication policy.
- **Provenance (criterion 13).** The entrypoint checks `/weights` against
  the initdata-pinned digest before serving. The public material does not
  document reproducible builds for the guest kernel, initrd or TDVF firmware,
  or supply a Dockerfile or signed provenance for the workload image.

### Evidence needed for a decision

Use a provider-issued bearer API key for an
[authenticated bridge probe](verification.md#tests-and-reproduction)
against `https://ctf.armet.ai` for `GLM-5.3-Flash`. Verify a fresh, non-debug
TDX quote with DCAP and record the measurements, TCB status and TDX module
identity. Confirm the quote-derived subject against the prediction and compare
the attested SPKI with the live TLS leaf. Require the bridge to return
`verified`, including the v2 `report_data` and signed, nonce-matched NRAS GPU
checks with the expanded payload. Repeat the report request with a fresh nonce
and a wrong model ID to check for `409`.

Request the following evidence from ArmetAI.

1. The decrypted `haproxy.cfg` or a reviewed disclosure of its routing,
   forwarding and logging, and confirmation that the external frontend uses
   TCP passthrough.
2. Confirmation that host-readable container output contains no prompts,
   completions or tool payloads, with the logging controls that enforce it.
3. An inventory of the keys Fortanix DSM releases, including the encrypted
   layers they unlock, the release policy and confirmation that DSM supplies
   no TLS private key.

If those checks and answers close the open findings, the proposed verdict is
**acceptable with conditions** covering measurement pins, pre-rollout
publication (criterion 7) and platform provenance (criterion 13).

### Other unverified items

The provider's initdata round-trip and policy tests were not run. The weights
manifest was not compared with a fresh Hugging Face download. The encrypted
API-key layers and parts of the certificate installation and ACME scripts
were outside the image review. Tool calling, streaming and tenant isolation
with `cache_salt` were last tested on vLLM and still need testing on SGLang.

## Historical self-assessment

Date: 2026-09-29 UTC. Updated: 2026-10-15 UTC.

> [!NOTE]
> This is the provider's **self-assessment** against the
> [audit criteria](../audit-criteria.md), submitted with the adapter. It is not
> an independent audit. Items marked **Provider evidence through live review session** are
> commitments that we want to show to a reviewer in a live review session to confirm against source and a live endpoint
> before a decision. Use [verification.md](verification.md) for the adapter
> algorithm, claims and limitations.

Source repos reviewed: **TODO** (guest image build, attestation service, and
in-TD TLS proxy repositories with commits, to be listed before review).

## Verdict

**Decision pending.** The adapter verifies the current live endpoint
`ctf.armet.ai` (see [New endpoint](#new-endpoint-ctfarmetai-2026-10-14) and
the [2026-10-15 re-probe](#re-probe-after-redeploy-2026-10-15)), and
earlier verified `poc1.armet.ai` (see [Live probe](#live-probe-2026-10-02) and
re-probes on [2026-10-07](#re-probe-after-redeploy-2026-10-07) and
[2026-10-09](#re-probe-after-redeploy-2026-10-09)). Source provenance, the
in-TD TLS key-generation and ACME code (pinned by digest in `initdata.toml`),
and the privacy boundary have not been reviewed.

The proposed trust model is:

1. The trust boundary is one **direct model instance**: a single Intel TDX TD
   on ArmetAI-operated KVM/QEMU hosts, serving one model at one HTTPS origin,
   with NVIDIA confidential-computing GPUs passed through to it.
2. The TD's quote commits, through `report_data`, to a fresh gateway nonce, the
   TLS SPKI it serves, the exact NVIDIA GPU evidence it collected, and the
   model ID.
3. The gateway pins the verified TD measurements and launch configuration
   against operator-configured `accepted_subjects`.
4. The GPU evidence must pass NRAS with a signed, nonce-matched result for
   every GPU.
5. The gateway pins the attested TLS SPKI on every forwarded request.

The expected decision, once the provider evidence below is supplied, is
**acceptable with conditions**. The conditions would be the required
measurement pins and the TODOs under criteria 7 and 13.

## Criteria

### 1. Workload identity

- **Met by the adapter:** DCAP-verified TDX quote, a fresh 32-byte nonce per
  verification, and debug TDs rejected. The identity is the pinned measurement
  subject over MRTD, RTMR0-3, TD_ATTRIBUTES, XFAM, MRCONFIGID, MROWNER and
  MROWNERCONFIG.
- **TLS key custody (since 2026-10-14):** the TLS private key is generated
  inside the TD and its certificate is obtained from inside the TD via ACME
  (Let's Encrypt). Fortanix DSM and CCM are no longer involved in TLS key
  release. On `ctf.armet.ai` the live certificate (Let's Encrypt `YE1`, ECDSA,
  `CN=ctf.armet.ai`) carries the TD-attested SPKI `d01db5a7…6e35`. The earlier
  `poc1.armet.ai` deployment used a DSM-released key; see the dated sections
  below.
- **Measurement of the key-generation code:** the TLS key-generation and ACME
  code is not part of the measured image. Between `poc1` and `ctf` the TLS
  implementation changed, but MRTD and RTMR0-3 are unchanged and only
  `MRCONFIGID` changed. ArmetAI states that `initdata.toml` pins that code by
  digest, so it is covered by `MRCONFIGID` and therefore by the gateway's pin.
  A change to the code changes the digest, `MRCONFIGID` and the subject, and
  fails closed until re-pinned.
- **Provider evidence through live review session:** the gateway cannot check
  custody itself. It only sees the SPKI the quote commits to. We will show:
  - the `initdata.toml` entries that pin the TLS key-generation and ACME
    component by digest, and that the TD refuses to run it unless the fetched
    artifact matches that digest;
  - that the key is generated in TD memory, never persisted outside the TD or
    exported, and that a relaunch produces a new key;
  - how the ACME challenge is answered from inside the TD.
- **WebPKI note:** whoever controls the `ctf.armet.ai` DNS or the HAProxy
  front end could also obtain a publicly trusted certificate for the name,
  with their own key. The gateway does not rely on WebPKI. It pins the
  TD-attested SPKI, so such a certificate fails the pin. Certificate
  Transparency monitoring for the domain is a useful extra signal.

### 2. Channel binding

- **Met by the adapter:** `tls_spki_sha256`, committed in `report_data`, is
  enforced on every forwarded request by the SPKI-pinned client. A certificate
  rotation fails closed until the gateway re-verifies.
- **Provider evidence through live review session:** TLS terminates inside the TD. No load
  balancer, CDN or ingress terminates TLS outside it.
- **Network path:** `ctf.armet.ai` (and earlier `poc1.armet.ai`) resolves to a
  server (`52.53.179.221`) running HAProxy in TCP passthrough mode, which forwards to
  the TD without terminating TLS. The live probe saw the TD-attested SPKI on
  the public origin. HAProxy is outside the trust boundary: SPKI pinning means
  it cannot read or alter traffic without the TD's private key, only drop or
  delay it. It sees connection metadata (client addresses, timing, sizes) but
  no content, and ArmetAI states it does not log that metadata.

### 3. Gateway soundness

Not applicable. The trust boundary is a direct model instance, not a router.
The adapter declares `model` scope and the gateway refuses a `router` scope
claim.

### 4. Model selection

- **Met by the adapter:** the gateway sends the canonical provider model ID.
  The report must echo it and `report_data` must commit to it, so an alias
  cannot select another TD's model.
- **Limitation:** the weights actually loaded are a property of the measured
  software; `model_weights_provenance` is Unknown.

### 5. Privacy boundary

**Provider evidence through live review session.** We plan to show the following, with source:

- No prompt, completion or tool payload logging, persistence, metrics or
  traces leave the TD.
- Billing events carry only token counts.
- No debug endpoints exist in production images.

### 6. Runtime policy

- **Met by the adapter:** debug TDs are rejected, and TD attributes and XFAM
  are pinned.
- **Launch configuration (`MRCONFIGID`):** the host sets `MRCONFIGID` to the
  hash of the TD's `initdata.toml`, the launch-time configuration it hands to
  the TD. `MRCONFIGID` is part of the pin, so any change to `initdata.toml`
  changes the subject and fails closed until re-pinned.
- **Exec disabled:** as of 2026-10-07, `initdata.toml` disallows exec into the
  TD. The change produced a new `MRCONFIGID` and subject (see
  [Re-probe](#re-probe-after-redeploy-2026-10-07)).
- **Hardened policy:** on 2026-10-15, `initdata.toml` was updated to harden
  the TD's runtime policy, producing a new `MRCONFIGID` and subject (see
  [Re-probe](#re-probe-after-redeploy-2026-10-15)). The specific changes will
  be shown with the published `initdata.toml` in the review session.
- **Provider evidence through live review session** We plan to show the following
  - the TD recomputes the hash of the `initdata.toml` it actually consumes and
    refuses to start unless it equals its own `MRCONFIGID`. Otherwise the host
    could hand the TD different initdata from the one the pin covers;
  - the published `initdata.toml` for each release, with the hash algorithm,
    so reviewers can recompute `MRCONFIGID` and judge its security-relevant
    settings;
  - an immutable, dm-verity-protected root filesystem measured through the
    kernel command line;
  - no SSH or remote shell;
  - credentials loaded only inside the TD;
  - no off-TEE TLS termination.

### 7. Release and measurement updates

**TODO. This blocks strict inclusion.** ArmetAI must publish, before each
production rollout:

- the release commit and image digest;
- the raw MRTD, RTMR0-3 and launch configuration fields, and the
  `initdata.toml` behind `MRCONFIGID`;
- the resulting `tdx-measurement` subject for each VM shape;
- a description of security-relevant changes.

It must also publish an emergency-rollout policy. The adapter fails closed on
any unpublished measurement. The 2026-10-07 `initdata.toml` change shows this
live: the previous pin was rejected until the new subject was pinned.

### 8. Lease lifecycle

Met by the gateway's standard per-model lease and non-destructive refresh. No
provider-specific session material is used.

### 9. Request fidelity

Chat and streaming pass end to end through the gateway (see
[Live probe](#live-probe-2026-10-02)). Tool calling passes with
`tool_choice: auto`, non-streaming and streaming, including a follow-up turn
with the tool result (see [Tool calling](#tool-calling-2026-10-09)).
**Provider evidence through live review session:** structured outputs, error
paths, and other `tool_choice` modes, as advertised.

### 10. Load balancing and cache

Per-instance endpoints: one TD per origin. There is no provider-side router in
the trust path. Cache affinity is not claimed.

### 11. Receipts

Met by the adapter. Evidence is the exact report bytes, with a `sha256:`
digest, recorded in the attested session.

### 12. Negative checks

Covered hermetically by `tests/provider_verifier/armet_ai_soundness.py`. It
checks replayed nonces, a swapped TLS key, another model, swapped GPU evidence,
each pinned field, debug, revoked TCB and the NRAS failure modes.

Live:

- A request for a model the TD does not serve (`GLM-5.3-Other`) was refused by
  the endpoint with `409`, and the verifier failed closed.
- After the 2026-10-07, 2026-10-09, 2026-10-14 and 2026-10-15 relaunches
  changed `MRCONFIGID`, the previous pin was rejected at the measurement step
  each time.

The certificate-rotation and VM-shape checks are **TODO**.

### 13. Source and platform provenance

**TODO. This blocks strict inclusion.**

- **Software provenance:** today the pin is operator trust in a measurement.
  A reproducible guest image build is required so reviewers can recompute the
  pinned values from source.
- **Platform provenance:** the TDX module `MR_SEAM` and `TEE_TCB_SVN` are
  recorded in provider claims but not pinned. Firmware (TDVF) provenance must
  be documented.

### 14. Platform TCB freshness

- **Met by the adapter:** `Revoked` is rejected. Other statuses are recorded,
  with advisory IDs, and surface as the hardware-proven `tcb_up_to_date`
  claim, the same bar as the other TDX adapters.
- **Provider commitment:** ArmetAI operates its own hosts and commits to
  keeping the TCB `UpToDate`, so relying parties can require it through claims
  policy.

## Live probe (2026-10-02)

The bridge verifier (`scripts/private_ai_provider_verifier.py`) was run
directly against `https://poc1.armet.ai` for provider model `GLM-5.3-Flash`,
then end to end through a locally run gateway (dstack simulator for the
gateway's own keys).

| Check | Result |
| --- | --- |
| Wire contract (v2 fields, nonce echo, `nvidia_payload` string) | Pass |
| Reported SPKI equals live certificate SPKI | Pass (`e3e15952…1cd5`) |
| DCAP quote verification with collateral | Pass; TCB `UpToDate`, no advisories |
| Debug TD | Off (`td_attributes` `0000001000000000`) |
| `report_data` commitment | Pass |
| Placeholder pin | Failed only at the pin step, as expected |
| Pinned run | `verified`, model scope, `tls_spki_sha256` binding |
| NRAS GPU gate | Pass: 8 × GH100, every GPU nonce-matched |
| Wrong model | `409` from the endpoint; verifier failed closed |

Observed values:

| Field | Value |
| --- | --- |
| Subject | `tdx-measurement:sha256:0265ed947a1c20c3ac84aae84c1a0088c215c46a390ae26575916cde58e788f6` |
| MRTD | `c78e2b8b…60b12a` |
| RTMR3 | all zeros (not extended) |
| MRCONFIGID | `a8b1759a…3c158b` (hash of `initdata.toml`) |
| MROWNER, MROWNERCONFIG | all zeros |
| MR_SEAM | `ab62561a…127f6b` |
| TEE_TCB_SVN | `0f010400000000000000000000000000` |

End to end through the gateway:

| Check | Result |
| --- | --- |
| Live E2E suite, quick profile | Pass: provider verification, SPKI-pinned forward, signed receipt, attested session |
| Receipt audit (`pap audit`, offline) | Receipt and `upstream-1` checks pass. `id-4` (the gateway's own source provenance) fails only because the gateway ran outside a dstack CVM. |
| Chat with `provider.aci_verified: true` | `200` with `X-Receipt-Id` |
| Streaming with `provider.aci_verified: true` | `200` with `X-Receipt-Id`, terminated by `[DONE]` |
| Pinned to a non-current `aci_session_ids` value | `412`, refused before forwarding |
| Session claims | `tee_attested`, `tcb_up_to_date`, `serving_software_known_good`, `gpu_attested` asserted; `os_known_good`, `model_weights_provenance` unknown |

Tools, structured outputs and error-path fidelity (criterion 9) are not yet
covered.

The subject was pinned for this test only. It becomes an acceptance decision
once the release behind it is published and reviewed (criteria 7 and 13).

## Re-probe after redeploy (2026-10-07)

ArmetAI relaunched the TD with an `initdata.toml` that disallows exec. The
verifier checks and the gateway end-to-end tests were rerun against
`https://poc1.armet.ai` for `GLM-5.3-Flash`.

| Check | Result |
| --- | --- |
| Wire contract and `report_data` commitment | Pass |
| Reported SPKI equals live certificate SPKI | Pass (`e3e15952…1cd5`, unchanged) |
| Previous pin (`0265ed94…88f6`) | Rejected at the measurement step |
| New subject, pinned for the test | `verified`; TCB `UpToDate`; NRAS 8 × GH100, every GPU nonce-matched |
| Wrong model | `409` from the endpoint; verifier failed closed |

Changed values; every other pinned field and the TDX module are unchanged:

| Field | Value |
| --- | --- |
| MRCONFIGID | `55a078a9…30a3bf` (was `a8b1759a…3c158b`) |
| Subject | `tdx-measurement:sha256:b2032b5a98bce6e73227959f456fd3b63a8958c45c61c24e86e69bc0cb8147b8` |

End to end through a locally run gateway, pinned to the new subject:

| Check | Result |
| --- | --- |
| Live E2E suite, quick profile | Pass: provider verification, SPKI-pinned forward, signed receipt, attested session |
| Receipt audit (`pap audit`, offline) | Receipt and `upstream-1` checks pass. `id-4` (the gateway's own source provenance) fails only because the gateway ran outside a dstack CVM. |
| Chat with `provider.aci_verified: true` | `200` with `X-Receipt-Id` |
| Streaming with `provider.aci_verified: true` | `200` with `X-Receipt-Id`, terminated by `[DONE]` |
| Pinned to a non-current `aci_session_ids` value | `412`, refused before forwarding |
| Session claims | `tee_attested`, `tcb_up_to_date`, `serving_software_known_good`, `gpu_attested` asserted; `os_known_good`, `model_weights_provenance` unknown |

The live E2E suite ran with the separate live-test session-label fix applied
(see the PR notes); without it, the suite's attested-session check fails for
any verified provider.

The new subject was pinned for this test only, as above.

## Re-probe after redeploy (2026-10-09)

ArmetAI relaunched the TD with a changed `initdata.toml`: the vLLM start
parameters now add `--enable-auto-tool-choice`, `--tool-call-parser glm47` and
`--reasoning-parser glm47`. Because the vLLM command line lives in
`initdata.toml`, this serving-configuration change is measured through
`MRCONFIGID` and required a new pin. The verifier checks and the gateway
end-to-end tests were rerun against `https://poc1.armet.ai` for
`GLM-5.3-Flash`. Tool calling was then tested separately (see
[Tool calling](#tool-calling-2026-10-09)).

| Check | Result |
| --- | --- |
| Wire contract and `report_data` commitment | Pass |
| Reported SPKI equals live certificate SPKI | Pass (`e3e15952…1cd5`, unchanged; released by Fortanix DSM) |
| Previous pin (`b2032b5a…47b8`) | Rejected at the measurement step |
| New subject, pinned for the test | `verified`; TCB `UpToDate`, no advisories; NRAS 8 × GH100, every GPU nonce-matched |
| Wrong model | `409` from the endpoint; verifier failed closed |

Changed values; every other pinned field and the TDX module are unchanged:

| Field | Value |
| --- | --- |
| MRCONFIGID | `ea7bfcb3…7fede6` (was `55a078a9…30a3bf`) |
| Subject | `tdx-measurement:sha256:f7bc92c48878e1318aea9cf33e23413aa0a9efc5dfc879b6e46b8c3d05db6f4b` |

End to end through a locally run gateway, pinned to the new subject, with the
same results as on 2026-10-07: the live E2E quick profile passes (with the
separate session-label fix applied); chat and streaming with
`provider.aci_verified: true` return `200` with `X-Receipt-Id`, the stream
terminated by `[DONE]`; a non-current `aci_session_ids` pin returns `412`; the
offline `pap audit` fails only `id-4`, the gateway's own source provenance; and
the session claims are unchanged.

The new subject was pinned for this test only, as above.

## Tool calling (2026-10-09)

Tested through a locally run gateway against the 2026-10-09 deployment (same
pinned subject), with `provider.aci_verified: true`, a single `get_weather`
function tool and `tool_choice: auto`. Every request returned `200` with a
signed receipt citing the verified ArmetAI session.

| Case | Result |
| --- | --- |
| Non-streaming | `finish_reason: tool_calls`; one `get_weather` call with valid JSON arguments (`{"city":"Paris","unit":"celsius"}`) and a tool-call id; reasoning returned separately from content |
| Streaming | Terminated by `[DONE]`; tool-call deltas reassemble into a valid `get_weather` call; `finish_reason: tool_calls` |
| Follow-up turn with the tool result | `finish_reason: stop`; the answer uses the returned data, with no further tool calls |

`tool_choice: required`, a named function, parallel tool calls and structured
outputs were not tested.

## New endpoint `ctf.armet.ai` (2026-10-14)

ArmetAI redeployed the model to a new origin, `https://ctf.armet.ai`, with a
new TLS implementation: the key is generated inside the TD and the certificate
is obtained via ACME (see [criterion 1](#1-workload-identity)). All verifier,
hermetic and end-to-end tests were rerun for `GLM-5.3-Flash`.

| Check | Result |
| --- | --- |
| Wire contract and `report_data` commitment | Pass |
| Reported SPKI equals live certificate SPKI | Pass (`d01db5a7…6e35`, new) |
| Certificate | Let's Encrypt `YE1`, ECDSA, `CN=ctf.armet.ai`, issued 2026-10-15 03:50 UTC |
| Previous pin (`f7bc92c4…6f4b`) | Rejected at the measurement step |
| New subject, pinned for the test | `verified`; TCB `UpToDate`, no advisories; NRAS 8 × GH100, every GPU nonce-matched |
| Wrong model | `409` from the endpoint; verifier failed closed |
| Hermetic soundness (ArmetAI, SecretAI) | Pass |
| `cargo test -p private-ai-gateway --all-targets` | 533 passed, 0 failed |

Changed values; MRTD, RTMR0-3, TD attributes, XFAM and the TDX module are
unchanged from `poc1.armet.ai`:

| Field | Value |
| --- | --- |
| MRCONFIGID | `4937f0a5…0ecd63` (was `ea7bfcb3…7fede6`) |
| Subject | `tdx-measurement:sha256:b954e6732ffe38e5c79901add18a20ffe9fc649b896806e283473e5017876420` |

End to end through a locally run gateway, pinned to the new subject:

| Check | Result |
| --- | --- |
| Live E2E suite, quick profile | Pass; the session records endpoint `https://ctf.armet.ai` and binding `d01db5a7…6e35` |
| Receipt audit (`pap audit`, offline) | Receipt and `upstream-1` checks pass; only `id-4` (the gateway's own source provenance) fails, as before |
| Chat with `provider.aci_verified: true` | `200` with `X-Receipt-Id` |
| Streaming with `provider.aci_verified: true` | `200` with `X-Receipt-Id`, terminated by `[DONE]` |
| Pinned to a non-current `aci_session_ids` value | `412`, refused before forwarding |
| Tool calling, `tool_choice: auto` | Non-streaming and streaming return a valid `get_weather` call (`finish_reason: tool_calls`); the follow-up turn with the tool result returns a final answer using it |
| Session claims | Unchanged: `tee_attested`, `tcb_up_to_date`, `serving_software_known_good`, `gpu_attested` asserted; `os_known_good`, `model_weights_provenance` unknown |

The live E2E suite ran with the session-label fix merged upstream in #400.
The new subject was pinned for this test only.

## Re-probe after redeploy (2026-10-15)

ArmetAI relaunched the TD on `ctf.armet.ai` with an `initdata.toml` updated to
harden the runtime policy. The tests were rerun for `GLM-5.3-Flash` on the
adapter rebased onto upstream `main` at `c98ec7b`, which includes the
Privatemode provider and the verifier-cache session listing (#399).

| Check | Result |
| --- | --- |
| Wire contract and `report_data` commitment | Pass |
| Reported SPKI equals live certificate SPKI | Pass (`a2e07c25…bb2c`, new: a fresh in-TD key for the new launch) |
| Certificate | Let's Encrypt `YE2`, issued 2026-10-16 02:27 UTC |
| Previous pin (`b954e673…6420`) | Rejected at the measurement step |
| New subject, pinned for the test | `verified`; TCB `UpToDate`, no advisories; NRAS 8 × GH100, every GPU nonce-matched |
| Wrong model | `409` from the endpoint; verifier failed closed |
| `cargo test -p private-ai-gateway --all-targets` | 621 passed, 0 failed |

Changed values; MRTD, RTMR0-3, TD attributes, XFAM and the TDX module are
unchanged:

| Field | Value |
| --- | --- |
| MRCONFIGID | `bc080317…d039aa` (was `4937f0a5…0ecd63`) |
| Subject | `tdx-measurement:sha256:da6f26e8c85dd6623946ab228f0f8f181a3b91b139c8277dcd6c79a8a13f0c8a` |

End to end through a locally run gateway, pinned to the new subject:

| Check | Result |
| --- | --- |
| Live E2E suite, quick profile | Pass |
| Receipt audit (`pap audit`, offline) | Receipt, `upstream-1` and `upstream-2` (the cited session verifies against the receipt) pass; only `id-4`, the gateway's own source provenance, fails, as before |
| `GET /v1/aci/sessions?model=armet-live` | Lists the current ArmetAI session |
| Pinned to the listed session via `aci_session_ids` | `200` |
| Pinned to a non-current `aci_session_ids` value | `412`, refused before forwarding |
| Chat and streaming with `provider.aci_verified: true` | `200` with `X-Receipt-Id`; the stream terminated by `[DONE]` |
| Tool calling, `tool_choice: auto` | Non-streaming, streaming and the tool-result follow-up turn pass |
| Session claims | Unchanged |

The new subject was pinned for this test only.

## Hard reject conditions

| Condition | Status |
| --- | --- |
| No enforceable channel binding | Clear: `tls_spki_sha256` enforced. |
| Plaintext leaves the boundary | Pending provider evidence (criteria 2 and 5). |
| Unverified model path | Clear: per-model TD, model committed in `report_data`. |
| Catalog metadata as proof | Clear: not used. |
| Aliases select an unrelated model | Clear: exact model ID match and commitment. |
| Debug bypass without pin change | Clear: debug rejected, TD attributes pinned. |
| Unpublished measurement swaps | Adapter fails closed; publication process is TODO (criterion 7). |
| Verifier fails open | Clear: every step gates, including GPU. |
| No model-scoped evidence | Clear. |
| Semantic post-processing | Chat, streaming and tool calling pass unmodified; structured outputs pending (criterion 9). |

## Open questions for maintainers

1. Should the pin also cover `MR_SEAM`, or is TCB status sufficient for TDX
   module updates?
2. Would maintainers prefer a provider-neutral name for this adapter shape
   (a non-dstack, measurement-pinned TDX endpoint with a mandatory GPU gate),
   so other providers can reuse it?
3. Is gating on `UpToDate` for this provider preferred over the cross-provider
   `Revoked`-only bar?
