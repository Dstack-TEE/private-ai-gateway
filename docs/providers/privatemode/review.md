# Privatemode provider review

Audit date: 2026-05-26 UTC. Provider behavior was rechecked against
Privatemode v1.48 and the live manifest on 2026-07-09. The gateway adapter was
changed to the measured co-deployment boundary on 2026-07-13 and to dynamic
manifest mode on 2026-07-31. The pinned v1.48 image was retested against live
traffic on 2026-09-23, and upstream source through v1.56 was rechecked. The
current deployment pins v1.58.0, rechecked on 2026-10-12 PDT.

> **Status (2026-09-29 standards pass):** each external interface was checked
> against the official v1.48.0 source and documentation. The readiness probe
> now uses the proxy's `/readyz` instead of `GET /v1/models`, which v1.48
> forwards unencrypted to the Privatemode API. The admin token is now a Compose
> secret instead of a parsed dstack internal file. `proxy_image_sha256` moved
> into a [published extension](../../../spec/proxy-image-binding.md). A fresh
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
- Expose only the gateway listener from the workload network. Version 1.58 of
  the official proxy supports `--listen-address` and defaults to `0.0.0.0`;
  the Compose keeps that default within its private network.
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
- Version 1.58 does not expose which manifest produced the active inference
  secret. Its manifest history is written before attestation and secret
  exchange complete. The gateway therefore reports the latest fetched digest
  but does not use it for manifest-specific claims.
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
through a fresh Phala deployment. A client E2EE v2 request over public ingress
returned an encrypted response that decrypted to `private-ok`; the signed
receipt/session audit had zero failures. Plaintext was also accepted, which
exposed the ingress trust-boundary gap; the deployment now sets
`require_client_e2ee`, which rejects plaintext and legacy E2EE. `pap verify` passed quote,
keyset, expiry, and measured-Compose checks but correctly failed `id-6` because
the public TLS key is not in the attested keyset.

The live manifest SHA-256 was
`f53011576c782d61912c884158a781ae5b55f3a49948e577c6292befc2535ab7`:
9 workload policies, one SNP profile, one TDX profile, and one seed-share
owner key. Its policy set differs from the July observation. This digest is a
proxy fetch-log observation, not proof that the request's active secret used
that manifest. No worker GPU/OS/weights claims are added on its basis.

Between source v1.48 and v1.58, the proxy gained listener/CORS and bounded
HTTP request handling. The client still verifies the mesh certificate and
signed hybrid HPKE exchange.
Full-body encryption and dynamic-manifest validation remain. The deployment
now pins the reviewed v1.58.0 image. Its background secret refresh retries
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

The [Phala verification on 2026-10-13][v158-live-evidence] supplies the live
acceptance evidence. A test CVM on the `dstack-0.5.9` production OS ran gateway
commit `ad238fad` and the pinned v1.58.0 proxy. The attested Compose equaled the
local render. `pap verify --require-production-os --accept-compose` passed
`id-1` through `id-4` and `policy-os`; only `id-6` failed, as expected on the
default Phala URL where TLS terminates outside the workload. Buffered and
streaming chat returned HTTP 200, and `pap audit --require-verified` had zero
failures, with `receipt-1` through `receipt-4` and `upstream-1` through
`upstream-2` passing. The session's `proxy_image_sha256` binding matched the
v1.58.0 image digest and credential digest, with an explicitly unbound dynamic
manifest observation. The test CVM was then deleted.

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
