# Tinfoil router-mode review

Date: 2026-11-23 PST (round 2).

> [!NOTE]
> This is a dated admission record. [Round 2](#round-2-2026-11-23-pst) checks
> deployed router `v0.0.179`. [Round 1](#round-1-2026-05-18-utc) records the
> original review. Source paths refer to the layout at the time of each round. Use
> [verification.md](verification.md) for the adapter algorithm, claims, and
> limitations.

Provider endpoint: `https://inference.tinfoil.sh`.
Router source: `tinfoilsh/confidential-model-router`.

## Round 2 (2026-11-23 PST)

Reviewed deployed router `v0.0.179` (commit
`0f1270114bd6066772a83540b8a491c0450b6e0d`), router image
`ghcr.io/tinfoilsh/confidential-model-router@sha256:6531d8e2981166da6248b7b52e8dbe8412eb591f7564bb5f6afba541cf9042bb`,
and CVM image `0.14.19`. The deployed tag is 201 commits and 76 releases
past round 1. The audit also read router `main` at `ea051bb` (PR #554),
which was outside the deployed release. [Router release](https://github.com/tinfoilsh/confidential-model-router/releases/tag/v0.0.179),
[measured config](https://github.com/tinfoilsh/confidential-model-router/blob/v0.0.179/tinfoil-config.yml),
[CVM image release](https://github.com/tinfoilsh/cvmimage/releases/tag/v0.14.19).

### Round 2 verdict

**Acceptable with conditions.** The gateway verifies the router's attestation
and pins its TLS key. The router verifies and pins the model replicas it
serves through. Strict-release inclusion requires a reviewed-release pin or
allowlist. Upgrade the gateway verifier SDK and keep cache locality unclaimed.

### Live check (2026-11-23 20:07–20:10 PST)

- The gateway bridge with `tinfoil==0.12.1` returned `verified` at router
  scope, with all four verification steps successful. The release digest
  was `63ccf36e517f064725749c9feaf5c0521f4a4e8603398db9d7877f357757dfbc`
  and the SEV-SNP launch measurement was
  `eb59905fbbb06279ebfd1d894c44591a69503c1c60a0fbd5ad59902a7af14fda9bca6221db7202b1ca23f9fdaf76008c`.
  Both match the published release. The `/health` and
  `/.well-known/tinfoil-proxy` endpoints reported `v0.0.179`.
  [Published measurements](https://github.com/tinfoilsh/confidential-model-router/releases/tag/v0.0.179).
- The TLS certificate's SPKI SHA-256 read with `openssl s_client` matched
  the attested value
  `901136ba338c1a4f64673c7582ed1ad08cc5d51fdf9fecffdb2299d629439926`.
- The v2 SEV-SNP report showed debug disabled (policy `0x30000`), reported
  TCB bl `0x0a`, snp `0x1b`, ucode `0x58`, and firmware 1.55 build 49.
  Its `report_data` contains the TLS fingerprint and HPKE key, with no nonce.
- Standalone Python SDK `0.15.0` (latest stable at the audit date) verified
  the same evidence and reported release tag `v0.0.179`. The
  `0.16.0rc3` prerelease verified the nonce-bound v3 document with the same
  release digest, measurement, and TLS fingerprint.
- The router's model listing showed three enclaves for `gpt-oss-120b`.
  The listing no longer includes `kimi-k2-6`.

### Changes since round 1

- Model-to-repository trust anchors are embedded in the measured binary. The
  measured `UPDATE_CONFIG_URL` points to Tinfoil's control plane. Runtime
  updates can change hostnames, overload limits, cache-route mode, and
  reservations, or remove a model from service. The router ignores
  repository changes and admits only models from the embedded config. The
  fetched config has no digest pin, leaving the control plane in charge of
  steering and availability.
  [Runtime update code](https://github.com/tinfoilsh/confidential-model-router/blob/v0.0.179/manager/manager.go).
- The router constructs a repository-pinned `tinfoil.NewEnclaveHTTPClient`
  for each replica. The router source describes TLS key pinning and
  re-verification when the enclave rejects the pinned key or verification
  expires. The router builds against `tinfoil-go v0.17.0-rc.11`, a release
  candidate. [Replica verification code](https://github.com/tinfoilsh/confidential-model-router/blob/v0.0.179/manager/verification.go),
  [Go dependencies](https://github.com/tinfoilsh/confidential-model-router/blob/v0.0.179/go.mod).
- The router can replay a chat, completions, or responses request once on
  another eligible verified replica of the same model after an initial
  backend 502, before it sends final response headers. Prompt bytes can
  therefore reach two attested enclaves.
  [Retry change](https://github.com/tinfoilsh/confidential-model-router/pull/538).
- The router implements rendezvous-hash cache routing. Its per-model mode
  comes from the unmeasured control-plane config, so cache locality remains
  unclaimed. [Cache routing code](https://github.com/tinfoilsh/confidential-model-router/tree/v0.0.179/cacheroute).
- The measured config sets `CACHE_SALT_ENABLED: "true"`. The router strips
  caller-supplied `cache_salt` from every parsed body. It injects a derived
  salt only on supported endpoints, using `user_cache_secret` as an input.
  [Cache salt code](https://github.com/tinfoilsh/confidential-model-router/blob/v0.0.179/cache_salt.go).
- Per-model subdomain routing was removed, superseding the round-1
  `X-Forwarded-Host` caveat. [Routing change](https://github.com/tinfoilsh/confidential-model-router/pull/490).

### Open findings

1. **Reviewed-release pin and advance publication.** The adapter resolves the
   provider's latest published release and checks repository provenance.
   It has no operator allowlist. Releases averaged about three per week
   between rounds. The `v0.0.179` release was published at 13:09 PST on
   2026-11-18, after the observed certificate's 12:25 PST validity start.
   The audit found no advance review window for this rollout.
   [Criterion 7](../audit-criteria.md#7-release-and-measurement-updates).
2. **Verifier currency and freshness.** The bridge's SDK pin, `0.12.1`,
   predates Sigstore hardening in `0.13.x` and release-tag binding in
   `0.14.0`. Its v2 report check binds the live TLS connection to the
   attested key. The provider's nonce-bound v3 path adds evidence freshness
   in the `0.16` prerelease.
3. **TCB floor.** The pinned SDK enforces a static minimum TCB well below the
   live platform's reported TCB. See the [claim mapping](verification.md#session-claims).
4. **Platform provenance.** The release names the CVM image version.
   A reviewed reproducible build pin is still pending.
5. **Selected-enclave receipts.** The gateway still does not record
   `Tinfoil-Enclave` in receipts.

### Verification limits

The audit relied on the SDK for Sigstore and Rekor checks. It did not rebuild
the router or CVM images, review the CVM image diff or Go SDK internals,
or exercise the `0.15.0` SDK through the gateway bridge. It did not repeat
the 2026-06 tamper tests or Rust session pinning tests. Retry, cache routing,
cache-salt enforcement, and the response header were assessed from source
without authenticated inference probes. A v2 retirement date remains unknown.

## Round 1 (2026-05-18 UTC)

Reviewed commit: `41b2e93e099baf3dd8085066c205f030b280cadc`.
Observed router image: `ghcr.io/tinfoilsh/confidential-model-router@sha256:893f9a33e3c24bced341413a2a934e9a4f0453a76787616d42631b3181c472ba`
(`v0.0.102`).

Source reports:

- [router-mode-soundness.md](../../reviews/router-mode-soundness.md)
- [router-mode-load-balancing-cache.md](../../reviews/router-mode-load-balancing-cache.md)

> **Status (2026-06 soundness pass):** the gateway now verifies Tinfoil with the official
> `tinfoil` SDK — the full AMD signature chain + Sigstore code-provenance + TLS-key
> binding — surfacing `release_digest`/`config_repo` (commit `747b117`). This review
> covers the Tinfoil router's own soundness; the strict digest pin/allowlist (P0 below)
> remains a follow-up.

### Verdict

Tinfoil router mode is acceptable with conditions.

The accepted trust model is:

1. The client verifies and pins the confidential router's attested TLS SPKI.
2. The attested router verifies each downstream model enclave before adding it
   to the serving pool.
3. The router forwards to the selected model enclave through TLS pinned to the
   model enclave's attested public key.

The main condition is that Private AI Gateway should pin the audited router
compose/image digest instead of accepting any future Tinfoil router
measurement implicitly. Router verification proves the HTTPS endpoint is a
TEE router; it does not by itself prove the router is the exact audited
source revision unless we bind and allowlist the source/image evidence.

### Criteria Status

Passed:

- The reviewed router verifies model enclave attestation before adding a model
  backend.
- Router-to-model forwarding uses TLS bound to the model enclave's attested
  public key.
- Tool/MCP routing follows the same attested enclave path.
- The normal production build did not include prompt/completion body logging in
  the reviewed paths.
- The router returns selected-enclave information in `Tinfoil-Enclave`, which
  can be recorded for audit.

P0 adapter requirements:

- Verify and pin the router TLS SPKI before forwarding.
- Verify Tinfoil's provider-published Sigstore measurements. The current
  Python verifier already does this for provider-current acceptance.
- Pin the audited router compose/image digest for strict-release runs, instead
  of implicitly accepting whatever Tinfoil publishes as latest.
- Do not set `X-Forwarded-Host` when forwarding through the router (superseded
  in [round 2](#round-2-2026-11-23-pst)).
- Treat `/v1/models` as advisory control-plane metadata, not proof.

P0 TODOs before strict-release inclusion:

- Maintain an allowlist tying router compose/image digest to reviewed source or
  release artifacts.
- Decide whether `UPDATE_CONFIG_URL` without a pinned hash is acceptable
  provider-owned policy or must be rejected in strict mode.
- Confirm production runtime flags cannot enable debug behavior without
  changing the pinned compose identity.

P1 TODOs:

- Record `Tinfoil-Enclave` in the session's `claims.extra` facts.
- Add live tests that verify the selected enclave belongs to the advertised
  model enclave set.
- Keep cache locality marked unclaimed unless Tinfoil adds observable
  cache-aware routing.

### Soundness Findings

Positive evidence:

- The router source is public and the reviewed deployment config pins a
  container image digest in `tinfoil-config.yml`.
- The router verifies model source measurements through Tinfoil's Sigstore
  chain before accepting a model enclave.
- `addEnclave` fetches `/.well-known/tinfoil-attestation`, verifies the
  attestation, verifies hardware measurements for TDX-Guest-V2, and rejects a
  model enclave whose measured source does not match the model's expected
  source measurement.
- Router-to-model forwarding uses
  `tinfoilClient.TLSBoundRoundTripper{ExpectedPublicKey: tlsKeyFP}`.
- Tool/MCP backends use the same attested model-enclave path.
- The production build excludes the developer-only on-disk session logger
  because `local_debug_disabled.go` is the default build path.
- Billing events carry token counts, model, route, request id, enclave, and
  API key metadata, but not prompt or response bodies.
- A grep of the reviewed source did not find prompt/completion body logging in
  the normal router, billing, or toolruntime paths.

Important caveats:

- Runtime config refresh uses `UPDATE_CONFIG_URL` without a required hash by
  default. A malicious or mistaken config change should not let an unverified
  enclave join the pool, but it can affect model-to-enclave steering and
  availability.
- `--debug` is a runtime flag. Enabling it would change compose arguments and
  therefore the measured compose hash, so a compose-hash allowlist should
  catch it.
- `X-Forwarded-Host` participates in subdomain model routing. Our gateway
  should not set it when forwarding to the router (superseded in
  [round 2](#round-2-2026-11-23-pst)).
- `/v1/models` uses the Tinfoil control plane and is not itself a TEE-bound
  user-content path.

### Load Balancing and Cache Findings

> This section records the round-1 algorithm. [Round 2](#round-2-2026-11-23-pst)
> documents the addition of cache-aware routing and cross-replica retry.

Tinfoil's router performs basic load balancing, not cache-aware routing.

Observed algorithm:

- `Model.NextEnclave(skip)` chooses uniformly at random among healthy,
  non-overloaded enclaves.
- It skips open circuit breakers, probes one half-open backend after cooldown,
  and prefers non-overloaded backends.
- The request path tries up to `EnclaveCount()` picks before returning 429.
- Once `enclave.ServeHTTP(w, r)` is called, the router does not retry against
  another backend. This is the right behavior for non-idempotent streaming
  completions.

Overload and health:

- Per-enclave metrics polling checks `vllm:num_requests_waiting`.
- Fresh queue-depth samples above threshold mark the backend overloaded.
- Stale metrics samples fail open and continue serving traffic.
- Three consecutive transport/backend failures open the circuit breaker; a
  single probe is allowed after 30 seconds.

Cache locality:

- There is no prefix trie, session key, prompt hash, or cache key in the
  router placement path.
- Multi-enclave models should be expected to spread identical prompts across
  instances.
- `kimi-k2-6` showed prompt-cache metadata only because it is currently a
  single-enclave model in the tested config.
- Private AI Gateway should not claim prompt-cache or KV-cache locality for Tinfoil
  router mode.

Backend observability:

- The router returns `Tinfoil-Enclave: <host>` on responses, including error
  paths. This is enough for the gateway to record which backend was chosen.

### Live Evidence

Live probes during review:

- `GET https://inference.tinfoil.sh/.well-known/tinfoil-proxy` returned the
  model/enclave map with measurements, `tls_key_fp`, and `hpke_key`.
- Multi-instance probes against `gpt-oss-120b` distributed across all three
  configured enclaves.
- Repeated `kimi-k2-6` requests exposed cached prompt token metadata only on
  the single configured enclave.

### Required Adapter Changes

P0:

- Surface and pin the router compose/image digest in the Tinfoil verifier
  result. The allowlist should be maintained by reviewing the corresponding
  router source/release.

P1:

- Record `Tinfoil-Enclave` in the ACI receipt event log or a provider-specific
  receipt extension. This makes the selected downstream enclave auditable.
- Decide whether to strip the raw `Tinfoil-Enclave` response header before
  returning to Private AI Gateway users, while still recording the selected enclave
  internally.

P2:

- Add an opt-in live test that sends repeated identical prompts to a
  multi-enclave Tinfoil model and verifies the observed router selection is
  not cache-affine.
- Add a live test that verifies `Tinfoil-Enclave` belongs to the model's
  advertised enclave set and that the enclave attestation matches the model
  source measurement.

### Source & platform provenance, and TCB status

Tracking criteria 13–14 of [audit-criteria.md](../audit-criteria.md):

- **Software provenance** (router/model code → reviewed source): mechanism done — the
  `tinfoil` SDK binds the code measurement to the repo via Sigstore (Rekor + GitHub
  workflow identity). **TODO:** pin to a *reviewed* release commit/digest allowlist, not
  any release of the configured repo (matches the P0 digest-pin item above).
- **Platform/OS provenance** (guest OS, kernel + cmdline, firmware/TEE module →
  reviewed reproducible build): partial — SEV policy (`MinimumTCB`, debug off) and
  Tinfoil's hardware measurements apply. **TODO:** pin the guest OS/firmware to a
  reviewed reproducible build.
- **TCB status / freshness**: done — the SDK enforces `MinimumTCB`.

### Open Questions

- Should we treat runtime config updates as acceptable provider-owned policy,
  or require a pinned update URL/hash for router-mode acceptance?
- Should the verifier bridge reject any Tinfoil router compose hash outside a
  known-good allowlist immediately, or only surface it for receipt/audit first?
- Should Private AI Gateway retain selected-enclave information only in receipts, or also
  expose it in a response header for debugging?
