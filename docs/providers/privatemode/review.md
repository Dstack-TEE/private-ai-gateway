# Privatemode provider review

Audit date: 2026-05-26 UTC. Provider behavior was rechecked against
Privatemode v1.48 and the live manifest on 2026-07-09. The gateway adapter was
changed to the measured co-deployment boundary on 2026-07-13.

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

Admission conditions:

- Pin a reviewed manifest digest instead of trusting automatic CDN updates.
- Pin the official proxy OCI image by digest in the same measured dstack
  Compose as the gateway.
- Mount the same reviewed manifest into both services and repeat its digest and
  proxy image digest in static gateway policy.
- Require every mutable Privatemode route to use the statically pinned internal
  service origin.
- Disable HTTP redirects for proxy readiness and forwarding requests.
- Bind the accepted credential digest in measured static policy and require a
  coordinated gateway/proxy redeploy to change it, matching the proxy's
  startup credential ownership semantics.
- Expose only the gateway listener from the workload network. Version 1.48 of
  the official proxy has no listen-address flag and opens its configured port on
  the network namespace's wildcard address.
- Override the proxy's availability-oriented NVIDIA OCSP defaults: reject
  unknown status and use no revoked-certificate grace period.
- Treat the proxy as part of the TCB: the gateway verifies its manifest binding
  but does not possess the provider E2EE secret.

## Verified trust chain

The observed chain was:

```text
SEV-SNP or TDX hardware evidence
  -> Contrast reference values in the pinned manifest
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

- The manifest publication channel has no detached signature. In automatic
  mode the initial trust seed is CDN TLS. The gateway adapter closes this gap
  operationally by requiring an explicit SHA-256 manifest pin and measured
  static-manifest mode.
- The observed manifest has one RSA seed-share owner key. Public ownership,
  recovery procedure, rotation policy, and quorum expectations should be
  documented.
- Privatemode does not expose a per-worker TLS SPKI or public E2EE key for the
  gateway to pin directly. Its binding is transitive through the manifest,
  attested Coordinator/Mesh CA, and secret-release protocol.
- The exact served worker is not named in a per-request signed receipt. ACI
  records the selected model and verified router-scoped manifest session.

## Live observations from the original audit

- The official proxy completed attestation and became ready in roughly ten
  seconds.
- A live chat request and an SSE streaming request succeeded over the encrypted
  path.
- A direct plaintext request to the public API returned HTTP 400 with the
  `privatemode-encrypted: false` signal.
- Streaming latency was stable in the sampled runs: time to first byte was
  approximately 0.24 seconds across the tested models, with low run-to-run
  throughput variance. These figures are operational observations, not
  security guarantees.

## Adapter validation

The adapter tests run a persistent HTTP service at the same boundary as the
Compose sidecar. They verify that manifest bytes are checked and retained at
static-policy construction, the actual shared credential file matches measured
policy, an unauthenticated internal model-list probe gates verified events, no
Bearer crosses the gateway-to-proxy hop, and both buffered and streaming
forwards reject v1.48's unencrypted handlers before network I/O. Remaining
forwards require the exact receipt binding, and receipts record the manifest,
Coordinator policy, credential digest, proxy image digest, and internal origin.
Config tests reject provider credentials, path overrides, and deployment fields
in mutable upstream config; deployment tests pin the official image while
ensuring its port is not published.

The earlier child-supervisor prototype was exercised end to end against the
production service on 2026-07-10, but that process-management boundary is no
longer part of the adapter. The replacement boundary was deployed and tested on
Phala Cloud on 2026-07-13:

- CVM `d0639110-1749-4c8f-9d1c-b49bb50afe32`, app ID
  `900ea355e7a448a2b27ad0a361eb6d71959bd8eb`, ran gateway commit
  `957b66aec31105a9fa6ca195536338443a24a055` under measured Compose hash
  `36b3611cdad52124bd5218bb4ccae84200c75dc91d9cc4bda045b79f2084fab1`.
- The official v1.48.0 proxy loaded the Clawdi-vault-backed API key from a
  Compose secret, verified the production SNP report, accepted the exact
  reviewed manifest, obtained its inference secret, and returned HTTP 200 from
  its proxy-authenticated outbound model-list request.
- A real `gpt-oss-120b` chat returned HTTP 200 and exactly
  `compose-sidecar-live-ok`. Signed receipt
  `rcpt-11a09da8994a59a5826d08a8` recorded the internal origin, manifest SHA-256
  `b4a4e1c372a507a1771f7f2f9b7c2fa7f04202855588e26f795d0249454572bf`,
  Coordinator policy
  `180d10463bdeccaf6c0ae6e0c01d26149f7cd1d2c1b2b4f3352224ef4510b9bf`,
  and proxy image digest
  `sha256:ff900b263a51a437633d15da809e7893a31fa4b1f4acfa4e526c075682d84307`.

A later v1.48 source trace found that an inbound Bearer is ignored once
`--apiKey` has initialized the proxy: the proxy overwrites outbound
`Authorization` with its static credential. The July 13 run therefore remains
valid transport and attestation evidence, but its receipt did not independently
prove the credential digest. The current adapter fixes that gap by mounting one
secret source into both services, validating its measured digest in the gateway,
sending no internal Bearer, and recording the credential digest in the channel
binding.

## Adapter decision

The gateway does not reimplement Contrast or copy only its quote checks. dstack
owns the official proxy lifecycle as a separate service in the same measured
Compose. The gateway verifier and forwarding backend share immutable static
policy for that service. Receipts bind the exact manifest and Coordinator
policy, shared credential digest, official proxy image digest, and internal
origin that carried the request. See [verification.md](verification.md) for the
enforced contract.
