# Privatemode provider review

The current reviewed version is Privatemode v1.64.0, checked on 2026-11-23 PST.
The reference deployment pins proxy OCI index digest
`sha256:cf046abd3f6cbccccc69cc742533607235b756cb497f8d23d4b02f42fc223334`.
The original audit was on 2026-05-26 UTC.

Provider behavior was rechecked against Privatemode v1.48 and the live manifest
on 2026-07-09. The gateway adapter changed to the measured co-deployment
boundary on 2026-07-13 and to dynamic manifest mode on 2026-07-31. The pinned
v1.48 image was retested against live traffic on 2026-09-23, and upstream source
through v1.56 was rechecked. The v1.64.0 source was checked on 2026-11-14 PST.

> **Currency check (2026-11-23 PST).** [Privatemode v1.64.0][v164-release],
> released on 2026-11-14 at tag commit `3c98e0ab`, remains the latest upstream
> release. The GHCR `v1.64.0` and `latest` tags both resolve to the pinned
> digest above.
>
> A fresh-nonce attestation from the production gateway passed `pap verify`
> checks `id-1` through `id-4` and `id-6`, with TDX TCB status `UpToDate`.
> The verifier skipped `id-5` because the command supplied no custody policy.
> The measured production Compose hash was
> `261760d004c92eab0b6bed59a55cf3214c65d2cb543a1bed6dff371cd2650a7a`.
> The production Compose pins the v1.64.0 proxy digest with
> `--api-key-fallback=always`, `--disable-prompt-cache=false`,
> `--nvidia-ocsp-allow-unknown=false`, and
> `--nvidia-ocsp-revoked-grace-period=0`.
>
> The check found no change to the deployed trust boundary, session binding,
> or adapter. It covered production gateway attestation. The latest
> per-session Privatemode receipt evidence is from the 2026-11-14 run below.
> The verdict remains **acceptable with conditions**.

> **Status (2026-09-29 standards pass):** each external interface was checked
> against the official v1.48.0 source and documentation. The readiness probe
> now uses the proxy's `/readyz` instead of `GET /v1/models`, which v1.48
> forwards unencrypted to the Privatemode API. The admin token is now a Compose
> secret. `proxy_image_sha256` moved into a
> [published extension](../../../spec/proxy-image-binding.md). A fresh
> production-OS Phala deployment passed `pap verify` with `--accept-compose` and
> served E2EE v2 chat and streaming with passing receipt and session audits.
>
> **Status (2026-09-30):** Privatemode now places no requirement on the client
> hop, like every other provider. The `require_client_e2ee` and
> `inference_token_sha256` settings were removed, so the deployment condition
> above about setting `require_client_e2ee` no longer applies.
> Sessions use the generic verifier-derived `tee_attested` claim.
>
> **Status (2026-10-10):** `main` now requires every sealed session to carry a
> complete evidence bundle. The manifest observation is that bundle for
> Privatemode, so it is required again: verification fails closed when no
> complete manifest can be read, and the previous complete entry is used while
> the proxy writes a new one.

> **Status (2026-10-12 PDT, v1.58.0 recheck):** the official OCI index digest is
> `sha256:5e8f549169c4288c24c902eadc4c6ebfaaf05c06e5605449c34469257929b143`.
> Tagged source confirms the same manifest-log path and line format, startup
> attestation/secret exchange before listening, `/readyz`, encrypted chat,
> completions and embeddings, and default plaintext forwarding of `/v1/models`.
> The Compose flags still work as deprecated aliases; `--listen-address` now
> exists with the same wildcard default. The Compose explicitly sets
> `--api-key-fallback=always` to preserve the measured startup credential behavior.
> No adapter behavior change is needed. Live acceptance passed on a production-OS
> Phala test CVM on 2026-10-13; see the [verification evidence][v158-live-evidence].

> **Status (2026-10-13 PDT, v1.59.0 recheck):** the official OCI index digest is
> `sha256:3cd4da0ee6ceee91ca84dde4e3e3f0e94e8b354d7c1d46987e4331a5c2f8bcff`.
> The [v1.58.0..v1.59.0 source comparison][v159-source-compare] and tagged
> [CLI][v159-cli], [manifest logger][v159-manifestlog],
> [setup][v159-setup], and [handlers][v159-handlers] confirm the same
> `<workspace>/manifests/log.txt` path and RFC 3339 timestamp/path format,
> startup attestation/secret exchange before listening, `/readyz`, encrypted
> chat, completions and embeddings, and plaintext `/v1/models` forwarding
> with the Compose defaults. The camelCase flag aliases remain supported,
> and `--api-key-fallback=always` still applies the startup credential.
> Removing the unused app/proxy client-header switch leaves this proxy's
> default header unchanged. No adapter behavior change is needed.
> Upstream moved its Contrast dependency (the Coordinator attestation library)
> from v1.24.1 to v1.25.0; the live Phala run covers this update.
> The [live test][v159-live-evidence] ran on the `dstack-0.5.9` production OS.
> The attested Compose equaled the local render.
> `pap verify --require-production-os --accept-compose` passed `id-1` through
> `id-4` and `policy-os`. Buffered and streaming chat returned HTTP 200.
> The receipt audit passed. The endpoint-binding check (`id-6`) failed in this
> test.
> The session binds the v1.59.0 proxy image digest and credential digest.

> **Status (2026-10-19 PDT, shared prompt cache):** slow first tokens and rate
> limiting in streaming traffic prompted a review of prompt caching. The tagged
> [CLI][v159-cli] and [handler][v159-handlers] confirm that the default
> `--disable-prompt-cache=true` generates a fresh salt for each unsalted request,
> preventing prefix reuse across requests. The reference Compose set
> `--disable-prompt-cache=false` to share the salt generated at proxy startup.
> A valid request `cache_salt` takes precedence. The effective salt and prompt
> content provide shard affinity. See [Prompt cache](verification.md#prompt-cache).
> The effect on time to first token, cache hits, and rate limiting was
> unverified at the time.

> **Status (2026-10-19 PDT, canonical proxy flags):** the reference Compose now
> uses `--api-key @<credential-file>`,
> `--nvidia-ocsp-allow-unknown=false`, and
> `--nvidia-ocsp-revoked-grace-period=0`. The tagged [CLI][v159-cli] registers
> the deprecated aliases with the same flag values and backing variables,
> including the same `@file` read and whitespace trimming for the credential.
> The canonical spellings preserve behavior, have no mutual-exclusion conflict
> with the other Compose flags, and remove deprecation warnings. Strict OCSP
> remains an admission condition: reject unknown status and allow no
> revoked-certificate grace period. The [proxy handler][v159-handlers] uses the
> inference secret to authenticate a per-request OCSP policy, and the
> [worker][v159-worker] verifies its MAC and enforces it against driver, GPU,
> and VBIOS statuses. The image digest and credential policy are unchanged.

> **Status (2026-11-05 PST, v1.62.0 local recheck):** the independently verified
> official OCI index digest is
> `sha256:b2d564aa5f51dfc3cee60424108ca3de1ed0b5349d67e7eac7efe17e62d68d5b`.
> The [v1.59.0..v1.62.0 source comparison][v162-source-compare] preserves the
> manifest-history path and format, startup attestation/secret exchange before
> listening, `/readyz`, and encrypted chat, completions, and embeddings. Contrast
> moves from v1.25.0 to v1.26.0; Go moves from 1.26 to 1.27. The proxy now carries
> strict OCSP policy in authenticated encrypted-request metadata, which workers
> verify before model forwarding. A downstream `Connection: close` no longer
> forces an upstream connection to be single-use. These changes are not an
> established fix for the observed HTTP/2 stalls.
> The exact Compose arguments plus `--help` passed on the pinned image with
> `--network none`, exit 0, empty stderr, and no deprecation warnings. The
> incoming timeout defaults are unchanged from v1.59.0; controlled real-image
> tests passed delayed headers and streaming at 130 seconds, including two
> 65-second stream gaps. See [Proxy timeouts](verification.md#proxy-timeouts).
> Minimal local live calls with the test key passed attestation/secret exchange,
> buffered `glm-5.3` chat (HTTP 200), and streaming chat (HTTP 200 with `[DONE]`),
> using 16 maximum output tokens each. Production rollout was unverified at
> the time.

> **Status (2026-11-14 PST, v1.64.0 manifest log):** the official OCI index
> digest is
> `sha256:cf046abd3f6cbccccc69cc742533607235b756cb497f8d23d4b02f42fc223334`.
> The manifest log now records the exchanged secret ID; see
> [verification.md](verification.md#what-the-manifest-observation-proves).
> The exact pinned image passed locally and on a throwaway Phala CVM with the
> test key: startup attestation, buffered and streaming `glm-5.3` calls, receipt
> audit with zero failures, and a secret rotation picked up by the gateway.
> Proxy responses carry no secret-ID header, so manifest binding stays false.
> Production rollout was pending at the time. See the 2026-11-23 currency check.

[v164-release]: https://github.com/edgelesssys/privatemode-public/releases/tag/v1.64.0
[v162-source-compare]: https://github.com/edgelesssys/privatemode-public/compare/v1.59.0...v1.62.0

[v159-live-evidence]: https://github.com/Dstack-TEE/private-ai-gateway/pull/430#issuecomment-6161932027
[v159-source-compare]: https://github.com/edgelesssys/privatemode-public/compare/v1.58.0...v1.59.0
[v159-cli]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/privatemode-proxy/cmd/cmd.go
[v159-manifestlog]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/privatemode-proxy/internal/manifestlog/manifestlog.go
[v159-setup]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/privatemode-proxy/internal/setup/setup.go
[v159-handlers]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/privatemode-proxy/internal/server/server.go
[v159-worker]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/inference-proxy/internal/adapter/inference/inference.go

Provider: [Privatemode](https://www.privatemode.ai/) by Edgeless Systems.
TCB source: [`edgelesssys/privatemode-public`](https://github.com/edgelesssys/privatemode-public).
Attestation framework: [`edgelesssys/contrast`](https://github.com/edgelesssys/contrast).

## Verdict

**Acceptable with conditions.** Privatemode has one of the strongest engineering
postures in the surveyed provider set. Its full encryption lifecycle was
reproduced against production: the client proxy verified the Coordinator,
obtained the attested Mesh CA, exchanged an inference secret, encrypted a live
chat request, decrypted the response, and streamed successfully. The public API
rejected a plaintext request.

Deployment conditions:

- Pin the official proxy OCI image by digest in the same measured dstack
  Compose as the gateway.
- Require every mutable Privatemode route to use the statically pinned internal
  service origin.
- Disable HTTP redirects for proxy readiness and forwarding requests.
- Bind the accepted credential digest in measured static policy and require a
  coordinated gateway/proxy redeploy to change it, matching the proxy's
  startup credential ownership semantics.
- Expose only the gateway listener from the workload network. The official
  proxy has supported `--listen-address` since v1.56, including in v1.64. The
  Compose keeps the `0.0.0.0` default within its private network.
- Override the proxy's availability-oriented NVIDIA OCSP defaults: reject
  unknown status and use no revoked-certificate grace period.
- Use the proxy's dynamic manifest mode so provider updates do not require a
  gateway redeployment.
- Surface the latest fetched manifest digest as explicitly unbound metadata.
  Do not derive manifest-specific claims until the proxy exposes the manifest
  bound to the active inference secret.
- Treat the proxy as part of the TCB. The gateway does not possess the provider
  E2EE secret.
- Protect the client hop like any other route. Behind Phala's public ingress,
  whose TLS ends outside the measured workload, set `require_client_e2ee`;
  behind dstack-ingress, the attested TLS binding suffices.

## Verified trust chain

The observed chain was:

```text
SEV-SNP or TDX hardware evidence
  -> Contrast reference values in the dynamically fetched manifest
    -> Coordinator policy admitted
      -> attested Coordinator supplies Mesh CA
        -> Secret Service authenticates under that Mesh CA
          -> inference secret released only to admitted workers
            -> official proxy encrypts request bodies to those workers
```

The manifest pins workload policies and hardware reference values. During the
original audit it included strict SNP guest policy and chip allowlists, TDX
measurements/platform identities, minimum TCB versions, and separate policies
for the Coordinator, Secret Service, and model workloads. The 2026-07-09 live
manifest still contained exactly one Coordinator policy, SNP and TDX reference
profiles, and one seed-share owner key.

Privatemode's model workers place decryption in an inference-proxy sidecar
inside the confidential VM. Plaintext is then passed locally to the model
server. The public edge sees encrypted bodies and routes them to workers.

## Criteria status

Passed:

- Workload identity is measured and admitted under Contrast policy.
- The inference secret is released through an attested Mesh-CA chain.
- CPU-TEE and GPU confidential-computing checks gate worker activation.
- Model workloads and deployment images are published in the TCB source.
- Builds are reproducible and releases are versioned.
- The public endpoint rejects unencrypted inference traffic.
- OpenAI-compatible chat, streaming, embeddings, models, and other documented
  surfaces are mediated by the official proxy.

Open or conditional:

- The manifest publication channel has no detached signature. Dynamic mode
  fetches through CDN TLS, then verifies that the Coordinator attestation
  matches the exact fetched manifest before accepting its Mesh CA.
- The observed manifest has one RSA seed-share owner key. Public ownership,
  recovery procedure, rotation policy, and quorum expectations should be
  documented.
- Privatemode does not expose a per-worker TLS SPKI or public E2EE key for the
  gateway to pin directly. Its binding is transitive through the manifest,
  attested Coordinator/Mesh CA, and secret-release protocol.
- Version 1.64 logs the secret ID for each successful exchange alongside its
  manifest. That log supports reporting the latest observed manifest digest
  and secret ID. A proxy response would need to identify the secret selected
  for the request before the gateway could make manifest-specific claims.
- The exact served worker is not named in a per-request signed receipt. ACI
  records the selected model and measured proxy session.

## Validation evidence

The original audit reproduced initial attestation, secret exchange, buffered
chat, and SSE streaming against production; a direct plaintext request to the
public API was rejected. The co-deployed v1.48 boundary was then exercised on
Phala Cloud on 2026-07-13 with a real `gpt-oss-120b` response and a signed
receipt.

On 2026-09-23, the same pinned image again verified a live Coordinator SNP
quote, exchanged a secret, and returned a real `gpt-oss-120b` completion
through a fresh Phala deployment. A client E2EE v2 request returned a response
that decrypted successfully. `pap verify` passed the quote, keyset, expiry,
and measured-Compose checks. The receipt audit passed. The endpoint-binding
check (`id-6`) failed in this test.

The live manifest SHA-256 was
`f53011576c782d61912c884158a781ae5b55f3a49948e577c6292befc2535ab7`:
9 workload policies, one SNP profile, one TDX profile, and one seed-share
owner key. Its policy set differs from the July observation. This digest is a
proxy fetch-log observation, not proof that the request's active secret used
that manifest. No worker GPU/OS/weights claims are added on its basis.

Between source v1.48 and v1.58, the proxy gained listener/CORS and bounded
HTTP request handling. The client still verifies the mesh certificate and
signed hybrid HPKE exchange.
Full-body encryption and dynamic-manifest validation remain. That deployment
pinned the reviewed v1.58.0 image. Its background secret refresh retries
failures with bounded backoff, while each encrypted attempt still calls
`LatestSecret` and rejects an expired secret that cannot be refreshed. The
active secret still is not bound to a manifest observation.

The v1.58 source also adds optional hosted-deployment controls: a local models
document can replace `/v1/models` forwarding, and API-key fallback can be changed.
The Compose leaves the models document unset and explicitly selects
`--api-key-fallback=always`, so `/v1/models` remains plaintext and the startup
API key still replaces outbound authorization. The gateway's three
allowed handlers all call `forwardEncrypted`; no new plaintext-forwarding
handler was found. See the tagged
[CLI](https://github.com/edgelesssys/privatemode-public/blob/v1.58.0/privatemode-proxy/cmd/cmd.go),
[handlers](https://github.com/edgelesssys/privatemode-public/blob/v1.58.0/privatemode-proxy/internal/server/server.go),
[setup](https://github.com/edgelesssys/privatemode-public/blob/v1.58.0/privatemode-proxy/internal/setup/setup.go), and
[secret manager](https://github.com/edgelesssys/privatemode-public/blob/v1.58.0/internal/oss/secretmanager/secretmanager.go).

On 2026-10-12 PDT, a local v1.58.0 Docker proxy verified a live Coordinator SNP
report, exchanged a secret, and served buffered and streaming `gpt-oss-120b`
chat through the gateway. Both returned HTTP 200. Exact response-byte hashes,
receipt signatures, cited session content addresses and evidence, and the
proxy-image/credential binding passed. The local offline audits failed `id-4`
because the official dstack v0.5.3 and v0.6.0 simulators' sample Compose bytes
disagree with their quotes' recorded Compose hash. This was a problem in the
simulator sample data.

The [2026-10-13 live test][v158-live-evidence] ran the pinned v1.58.0 proxy on the
`dstack-0.5.9` production OS. The attested Compose equaled the local render.
`pap verify --require-production-os --accept-compose` passed `id-1` through
`id-4` and `policy-os`. Buffered and streaming chat returned HTTP 200.
The receipt audit passed. The endpoint-binding check (`id-6`) failed in this
test. The session's `proxy_image_sha256` binding matched the
v1.58.0 image digest and credential digest, with an explicitly unbound dynamic
manifest observation.

[v158-live-evidence]: https://github.com/Dstack-TEE/private-ai-gateway/pull/410#issuecomment-6136973775

A later source trace showed that the proxy replaces inbound authorization with
its startup credential. The current adapter therefore sends no internal Bearer,
validates the shared Compose credential digest itself, and records that digest
in the enforced binding. Tests cover the proxy-image and credential pins,
dynamic manifest observation, bounded readiness probe, redirect refusal,
encrypted-handler allowlist, buffered and streaming paths, and receipt binding.
See [verification.md](verification.md) for the current contract and its
freshness limits.

## Adapter decision

The gateway does not reimplement Contrast or copy only its quote checks. dstack
owns the official proxy lifecycle as a separate service in the same measured
Compose. The gateway verifier and forwarding backend share immutable static
policy for that service. Receipts bind the shared credential digest, official
proxy image digest, and internal origin that carried the request. They expose
the latest manifest-log entry as unbound evidence. See
[verification.md](verification.md) for the enforced contract.
