# Chutes E2EE review

Date: 2026-11-23 PST (round 2).

> [!NOTE]
> This dated admission record retains the evidence and decision for each
> round. Source paths refer to the repository layout at the time. Use
> [verification.md](verification.md) for the adapter algorithm, claims, and
> limitations.
>
> [Round 2](#round-2-2026-11-23-pst) reviews the deployed sek8s guest images
> 1.4.0, 1.4.1, and 1.5.0. The [May review](#round-1-2026-05-18-utc) is
> retained below.

Provider endpoint: `https://api.chutes.ai`.
Trust boundary: verified Chutes model instance reached through Chutes E2EE
transport.

## Round 2 (2026-11-23 PST)

The deployed TDX fleet runs sek8s guest images 1.4.0, 1.4.1, and 1.5.0.
The latest release at the time of review was 1.5.0 (`a526aef4`, 2026-10-11).
[sek8s v1.5.0 release](https://github.com/chutesai/sek8s/releases/tag/v1.5.0)

### Verdict

Chutes remains **accepted for limited traffic** on the E2EE path. The trust
boundary is the verified model instance. The first 32 bytes of TDX
`report_data` bind the evidence nonce to the instance's E2EE public key. The
gateway restricts encrypted requests to keys covered by a verified lease.
Nonce discovery throughput and unbound served weights remain limits on use.
[Chutes verification](verification.md)

### Live check (2026-11-23 20:07–20:13 PST)

- The legacy measurement endpoint returned 63 profiles across 1.3.1, 1.4.0,
  1.4.1, and 1.5.0. Both TDX measurement endpoints returned the same 60 distinct
  MRTD/RTMR tuples, all with MRTD `261CE538…BBEE8D`.
  [Legacy feed](https://api.chutes.ai/servers/tee/measurements),
  [TDX feed](https://api.chutes.ai/servers/tdx/measurements)
- Public evidence requests with fresh nonces covered the 11 hosted TEE chutes
  owned by the `chutes` user. All 146 returned quotes verified with `dcap-qvl`
  0.3.13, reported TDX v4 and `UpToDate`, had debug disabled and no advisories,
  and matched a published profile. The evidence endpoint also reported eight
  collection failures for `Qwen/Qwen3.8-27B-TEE`.
- The returned quotes matched 1.5.0 on 58 instances, 1.4.1 on 86, and 1.4.0 on
  two. The accepted 1.3.1 profiles had no matches in this sample.
- The SNP measurement feed returned an empty list. The 1.5.0 image supports
  AMD SEV-SNP, and its release notes say the network has yet to admit SNP guests.
  [SNP feed](https://api.chutes.ai/servers/snp/measurements),
  [sek8s v1.5.0 release](https://github.com/chutesai/sek8s/releases/tag/v1.5.0)
- The production gateway's public session listing shows Chutes sessions verified
  with the current adapter. The direct probe received HTTP 401 from E2EE
  instance discovery, which requires a Chutes credential. The direct check
  therefore left E2EE key binding, encrypted invocation, streaming, and live
  discovery limits untested. The deployed Chutes API revision and the gateway's
  production route pins were outside the evidence collected.
  [Public session listing](https://tee.redpill.ai/v1/aci/sessions)

### Provenance and open findings

1. **Platform provenance.** Chutes reports reproducible guest image measurements
   in the sek8s v1.4.0 release notes and publishes a rebuild guide for v1.5.0.
   This review did not rebuild either image. The public measurement list still
   includes 1.3.1, which predates the documented reproducible build.
   [sek8s v1.4.0 release](https://github.com/chutesai/sek8s/releases/tag/v1.4.0),
   [v1.5.0 rebuild guide](https://github.com/chutesai/sek8s/blob/v1.5.0/docs/reproducing-measurements.md)
2. **Measurement admission.** The adapter relies on the provider's live list.
   The public Chutes API source permits authorized use of release-candidate
   profiles, including those for debug builds, and omits them from the public
   feed. The adapter requires a match in the public list and independently
   rejects a quote with the TDX debug bit set. The sek8s guide marks the legacy
   endpoint used by the adapter as deprecated.
   [Chutes public feed source](https://github.com/chutesai/chutes-api/blob/3b5609f4/api/server/router.py#L485-L522),
   [Chutes release-candidate policy](https://github.com/chutesai/chutes-api/blob/3b5609f4/api/server/util.py#L1235-L1273),
   [measurement guide](https://github.com/chutesai/sek8s/blob/v1.5.0/docs/reproducing-measurements.md)
3. **Release process.** sek8s publishes tagged source and release notes with
   security-relevant changes. The reviewed release carries no reference
   measurement values. This check could not establish whether Chutes publishes
   measurement changes ahead of rollout as required by
   [criterion 7](../audit-criteria.md#7-release-and-measurement-updates).
4. **Nonce throughput (P0).** The public Chutes API source at `3b5609f4`
   caps discovery at five instances and ten nonces per instance, with a client
   TTL of 60 seconds. The deployed limits and HTTP 429 behavior were not
   re-measured. [Chutes E2EE discovery source](https://github.com/chutesai/chutes-api/blob/3b5609f4/api/e2e/router.py#L66-L76)
5. **Served-weight identity (P0).** Chute display names can alias weights
   identified only in unsigned metadata. For example, the catalog `readme`
   for `zai-org/GLM-5.2-TEE` names `nvidia/GLM-5.2-NVFP4`.
   [Public catalog entry](https://api.chutes.ai/chutes/?name=zai-org%2FGLM-5.2-TEE)
6. **Catalog changes.** The public catalog no longer resolves
   `moonshotai/Kimi-K2.5-TEE`. Current hosted TEE chutes include
   `moonshotai/Kimi-K3-TEE`, `moonshotai/Kimi-K2.6-TEE`,
   `Qwen/Qwen3.8-27B-TEE`, and `zai-org/GLM-5.2-TEE`. Display names can
   still repeat. Routes configured by model name should pin the concrete chute
   UUID through `chutes_chute_ids`. [Chute ID configuration](configuration.md#field-behavior)

Chutes also publishes a separate E2E v2 protocol for customer-built TDX VMs.
This review covers the hosted E2EE adapter. [Chutes TDX gate](https://github.com/chutesai/chutes-tdx-gate)

## Round 1 (2026-05-18 UTC)

Source reports:

- [upstream-verification-lifecycle.md](../../upstream-verification-lifecycle.md)
- Chutes catalog and E2EE throughput probes captured during the provider audit.

> **Status (2026-06 soundness pass):** Chutes verification confirmed sound — DCAP quote
> signature verification plus `report_data ↔ SHA256(nonce ‖ e2e_pubkey)`; no change
> required.

### Verdict

Chutes is accepted for limited traffic on the E2EE path.

The security model is direct instance binding:

1. The Chutes verifier verifies TDX evidence for a model instance.
2. The quote report data binds freshness and the instance E2EE public key.
3. Private AI Gateway records the E2EE public-key digest as the channel binding.
4. The Chutes backend encrypts the request to a nonce-bearing instance whose
   key digest is covered by the current verification lease.

This is sound for confidentiality and request-path binding when the adapter
uses the E2EE transport. It is not yet ready for general production throughput
because Chutes' public nonce discovery endpoint exposes only a small,
rate-limited subset of instances per call.

### Criteria Status

Passed:

- E2EE public-key material is bound to the attestation evidence.
- Private AI Gateway enforces the E2EE public-key digest before invoking Chutes.
- Nonces are treated as single-use local session material.
- Background verification and session refresh keep cold evidence verification
  off the normal request path while the caches remain warm.
- Streaming responses are decrypted before normal ACI receipt hashing.
- Chutes catalog lookup now requires an exact model-name match, or a UUID-like
  `chute_id` supplied directly. It no longer falls back to the first returned
  catalog item.
- Production routes can pin configured upstream model ids to concrete
  `chute_id` UUIDs with `chutes_chute_ids`; both the verifier bridge and E2EE
  sender use the same pin before any catalog lookup.

P0 TODOs:

- Resolve the nonce-throughput ceiling before routing meaningful user traffic.
  Current discovery returns at most five instances and ten nonces per instance
  per call, with aggressive 429 behavior.
- Do not claim exact served-weight identity beyond the verified Chutes claim
  unless Chutes exposes a signed or otherwise verified `served_model_id`.

P1 TODOs:

- Add long-window throughput tests across many nonce TTL cycles.
- Expose pool metrics: verified key count, pooled nonce count, refresh
  success/failure, and binding mismatch count.
- Record the Chutes instance id and key id in provider-specific receipt claims
  when available.

### Workload Identity And Binding

Chutes does not expose a separate durable ACI-style identity key for each model
fleet. The practical identity for Private AI Gateway is the verified instance E2EE key.

The verifier expects the TDX report-data field to bind the verifier nonce and
E2EE public key:

```text
report_data[0:32] = sha256(nonce || e2e_public_key)
```

The derived resource is the encrypted invocation. A request is acceptable only
when it is encrypted for an instance key whose digest is present in the current
verified lease.

### Catalog And Model Identity

The public Chutes catalog is not a security root.

Findings:

- Most TEE-flagged catalog entries are inactive miner workloads.
- Production traffic is concentrated in a small set of chutes operated by the
  canonical `chutes` user.
- `name` is not globally unique across the full catalog.
- The current resolver's "first item with any `chute_id`" fallback is unsafe.
- Some display names alias different served weights or quantizations. The
  public API did not expose a signed machine-readable served-model id in the
  snapshot.

Adapter rule:

```text
Exact configured model -> exact chute id -> verified E2EE instance key.
No exact match, no lease.
```

### Session And Throughput

Chutes E2EE uses two freshness layers:

- evidence freshness for verifying instance keys
- invocation nonces for sending encrypted requests

Invocation nonces behave like short-lived tickets. They do not replace
attestation. They are usable only when their instance key is already verified.

Observed discovery behavior:

- `/e2e/instances/{chute_id}` returned at most five instances per call.
- Each instance returned ten nonces.
- `nonce_expires_in` was 60 seconds.
- Polling too aggressively caused 429s on busy chutes.

This makes the high-throughput path provider-limited. The current Private AI Gateway
implementation can refresh a nonce pool, but it cannot turn a
five-instance discovery window into full-fleet load balancing.

### Privacy Boundary

The accepted Chutes path is application-level E2EE. Private AI Gateway forwards encrypted
request material to Chutes and decrypts the response in the gateway before
normal ACI receipt hashing.

Plain HTTPS to Chutes is not accepted for ACI-secured traffic unless Chutes
provides a separate attested TLS binding or equivalent router proof.

### Required Adapter Behavior

The Chutes adapter must:

- run provider-owned verification during background refresh
- record only E2EE key digests that passed verification
- pop only non-expired, locally unused invocation nonces
- match every nonce to a verified key digest before invocation
- fail closed when no verified nonce is available
- keep credential material in the upstream config, not process environment
- record E2EE binding and compact provider claims in receipts

### Source & platform provenance, and TCB status

Tracking criteria 13–14 of [audit-criteria.md](../audit-criteria.md):

- **Software provenance** (model/server code → reviewed source): partial — the TDX
  measurement is matched against a reviewed public profile (`chutes_measurement_name`),
  but the served-model identity is **not** bound (the chute display name aliases the
  actual weights via unsigned `readme` text). **TODO:** maintain/pin the reviewed
  measurement profile and bind the served model.
- **Platform/OS provenance** (guest OS, kernel + cmdline, firmware/TEE module →
  reviewed reproducible build): **TODO** — `MR_TD`/RTMRs are not pinned to a reviewed
  reproducible OS/firmware build.
  _Update (2026-11-23 PST): Chutes now publishes a reproducible build guide. See
  [round 2](#round-2-2026-11-23-pst)._
- **TCB status / freshness**: done — the bridge requires `UpToDate`.
  _Update (2026-06-12): the bridge no longer rejects a stale TCB. It records the
  status, and the session layer refutes `tcb_up_to_date` for any status other
  than `UpToDate`; see [verification.md](verification.md)._

### Open Questions

- Is the five-instance discovery cap intentional, and is there pagination or a
  bulk high-throughput path?
- Can Chutes return signed served-model identity or structured weight metadata?
- Can Chutes expose stable per-instance evidence TTLs and key-rotation rules?
- Should Private AI Gateway support a Chutes managed-router path if Chutes later provides
  attested router TLS binding and full gateway soundness?
