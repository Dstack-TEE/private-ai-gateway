# 0G Private Computer admission review

Review date: 2026-10-14 UTC; updated 2026-10-16 UTC against the latest source.
Status: draft, documentation-only assessment.

Provider: [0G Private Computer](https://pc.0g.ai/).
Public client/gateway source: [`0gfoundation/0g-pc-e2ee`][source].
Reviewed source: `4208145aaae06541568251f3f65b8987a10fe84f` (`main`, updated
from `e320272f41c7cb349e8d3c1c3c4dfa9a8570414b`).
Reviewed gateway release: `release-2026.09.28.1`, commit
`5de8d865bd0f69168fed76711f065a898a6015c6`; it remains the latest release.
The cited resolver, per-model sealing dispatch, stream handling and
`ZG_GATEWAY_SEAL_MODELS` default are unchanged on `main`. `main` also adds
sealed asynchronous image generation, which is outside this chat-completions
review.
PAG integration seams were inspected at
`57477f5d9caced1f95ffd2562e7ba421849a0d7e`.

## Decision and scope

**Not acceptable for strict verified-provider admission at the reviewed
revisions.** The OpenAI-compatible API and public encryption client provide a
practical integration path. Admission requires an enforced, reviewed provider
application identity and a verifiable per-model sealing policy.

This review applies the [provider audit criteria](../audit-criteria.md),
particularly gateway soundness, privacy boundary and runtime policy. Its goal
is end-user-verifiable privacy: public privacy-relevant source and build
provenance must correspond to accepted runtime identities and channel keys.

This PR adds review documentation and a proposed implementation path. Provider
registration, verifier code, transport changes and production activation are
future work. All references below are public. Deployment addresses, account
identifiers, credentials, raw live evidence and private discussion are excluded.

## Existing controls and evidence boundary

The public implementation provides:

- OpenAI-compatible chat completions and SSE, with `glm-5.3` as an upstream
  model identifier.
- A gateway with dstack-ingress TLS, published release Compose and
  [verification instructions][gateway-verification].
- Provider CPU-TEE quote verification, OS boot-chain allowlisting, encryption
  and signing keys bound into quote `report_data`, and on-chain provider
  signer checks. See the [trust-chain implementation overview][trust-controls].
- HPKE request/response transport and optional signed-response verification.
  The inspected release Compose explicitly enables attestation enforcement,
  on-chain enforcement and response verification.

Read-only checks compared the public gateway Compose with its published release,
compared the served TLS leaf with the evidence certificate, and recomputed
certificate-manifest, raw quote report-data and provider app-compose hash
bindings. These checks establish byte correspondence. Independent DCAP/TCB
validation, GPU validation, authenticated inference and adapter tests remain
acceptance work. Reproducible-build verification also remains open.

## Admission blockers

### 1. Provider application admission must gate encryption

The resolver verifies a provider quote and returns its encryption key and
signer. It reads the app-compose for a container display; missing, mismatched or
unreadable Compose data yields an empty display while key use remains possible.
See [`verifyQuoteAt` and `containersOf`][route-compose].

Since the first revision of this review, 0G has added application checks to its
standalone `pcverify -provider` verifier, which runs outside the request path:

- The [application baseline][baseline] now records two reviewed deployments,
  compared per service and per top-level section.
- Broker images may change only to releases with signed SLSA provenance from the
  broker release workflow at or above a minimum version.
- An RTMR3 event-log replay checks that the image swapped in after boot is an
  accepted release bound to the quote's keys.

These checks are not in the sealing path. The provider's
[trust-chain documentation][application-gap] states that wiring them in waits
on runtime upstream rules and on the fleet moving to an accepted broker release;
it reports that enforcing today would refuse every mainnet provider, which run
unreleased builds swapped in after boot. A provider without a recorded deployment
is reported as not compared rather than failed, and recording a deployment is
not approval of its manifest. OS admission, signer registration and application
admission remain separate checks.

**Conditional malicious-provider scenario:** a registered operator boots an
accepted OS and runs a modified broker that copies decrypted prompts. The
application commitment changes, while genuine CPU attestation and signer
registration can still pass. The resolver at both reviewed revisions permits
this application identity to supply a sealing key.

**Done when:** public source/build provenance covers the plaintext-handling
broker and serving path; reviewed policy covers images, startup commands,
mounts, logging, controllers and administrative access; an application
allowlist gates key use before prompt encryption. Missing, changed or
unapproved application evidence must reject the candidate. Policy must also
cover mutable files, configuration and image swaps after boot. The baseline,
release-provenance and RTMR3 checks now in `pcverify` are a basis for this gate
once they run in the sealing path with an enforce switch.

### 2. Sealing must be enforced for the admitted model

The [release Compose][release-compose] defaults `ZG_GATEWAY_SEAL_MODELS` to
`0gm-1.0-35b-a3b`. The application permits an injected environment override of
that list. The [gateway's per-model dispatch][sealing-policy] forwards models
outside the selected sealing set through its unsealed router path. Public policy shape
alone leaves the injected model list unknown.

`X-0G-Provider-Trust-Mode: private` selects the documented TeeML provider tier.
Gateway-to-provider sealing is an additional requirement; see the
[privacy-mode documentation][privacy-mode].

**Done when:** the admitted deployment binds the selected model to an enforced
sealing policy. Disabling sealing, removing the model from the set or selecting
an unsealed route must fail before prompt transmission. A locally measured
sidecar can own this policy instead of relying on a hosted gateway override.

### 3. Establish the model-serving and GPU boundary

Treat catalog tiers as discovery hints. Admission must trace the selected
provider's actual target, including any external delegation, and bind it to the
reviewed model-serving workload. An independent upstream route also needs
loop prevention when a provider delegates back to the integrating gateway.

The inspected client establishes CPU-TEE/key identity. This review leaves GPU
attestation enforcement and the measured CPU-to-GPU serving channel open.
A returned NVIDIA payload supplies material for verification; admission needs
a checked GPU identity/status and reviewed serving-channel enforcement.

**Done when:** the admitted route has a reviewed plaintext path through the
model service, protected CPU/GPU communication and verified GPU policy. Publish
source/artifact provenance for that path and state the exact model and
quantization claims supported by its evidence.

## Proposed PAG integration

### Preferred: measured local E2EE sidecar

Co-deploy the 0G client/proxy in PAG's measured workload:

```text
PAG -> local 0G client -> encrypted routing hop -> admitted provider/model
```

- Pin public sidecar source/build provenance and image identity in measured
  deployment policy. Expose the PAG listener; keep the sidecar on the internal
  workload network with a fixed origin and redirect rejection.
- Enforce application admission, private-tier selection, provider identity,
  per-model sealing, quote/TCB policy and key binding before sending prompts.
  Encrypted-transport failures must terminate the request.
- Reuse the provider's HPKE and proof implementation. Extend PAG's provider
  configuration, verifier/session claims and request backend with an explicit
  measured-proxy binding. The existing
  [Privatemode integration](../privatemode/verification.md) provides an
  architectural reference; 0G requires its own policy and evidence contract.
- Bind the admitted provider, model, policy version and evidence digest into
  the session. State which checks PAG performs and which the measured sidecar
  enforces. Lease expiry and refresh must retain the existing fail-closed rules.

### Alternative: verify the hosted 0G gateway

Add a provider-specific verifier that validates gateway quote freshness/TCB,
release/runtime policy and certificate binding. Return the existing
`tls_spki_sha256` binding to PAG's OpenAI backend. Add provider-specific routing
headers and model mapping. The hosted gateway must enforce the downstream
application and sealing requirements above; pinning its TLS identity alone
leaves those admission obligations open.

Neither path requires a new ACI wire protocol or a Rust port of provider
attestation cryptography.

## Streaming and receipt semantics

Responses are sealed with HPKE base mode to the client's ephemeral public key,
which the request carries readably. Base mode does not authenticate the sender:
an intermediary that reads that key can produce response frames the client
opens successfully. Upstream [documents this][response-auth]; only the provider
signature binds response bytes to the admitted provider, and response
verification is off by default.

In the inspected [stream implementation][stream], each frame passes HPKE/AEAD
opening before delivery, and the aggregate provider signature is fetched and
verified at stream completion. Early frames are therefore provisional and not
authenticated to the provider. Admission requires response verification for
buffered and streamed responses. Propagate final verification failure and issue
a success receipt only after all required checks complete. Buffer the response
when authenticated content is required before consumption or tool execution.

## Implementation acceptance checklist

- [ ] Independently verify CPU quote freshness, collateral/TCB and channel keys.
- [ ] Review public application/build/runtime policy and enforce its allowlist.
- [ ] Verify GPU evidence and the actual protected model-serving channel.
- [ ] Reject missing/changed application, changed key, disallowed provider,
      unsealed model, expired lease and unavailable admitted capacity before
      sending plaintext or sealing to an unapproved key.
- [ ] Exercise buffered chat and complete SSE with real provider inference.
- [ ] Test corrupted ciphertext, reordered/truncated frames, disconnects,
      signature-fetch failure, invalid final signature, and well-formed
      responses sealed by a party other than the admitted provider.
- [ ] Verify model mapping, request parameters, usage accounting and the
      receipt/session evidence contract.
- [ ] Publish candidate release measurements/policy before rollout and define
      refresh, revocation and rollback behavior.

[source]: https://github.com/0gfoundation/0g-pc-e2ee/tree/4208145aaae06541568251f3f65b8987a10fe84f
[gateway-verification]: https://github.com/0gfoundation/0g-pc-e2ee/blob/5de8d865bd0f69168fed76711f065a898a6015c6/docs/verifying-the-gateway.md
[trust-controls]: https://github.com/0gfoundation/0g-pc-e2ee/blob/5de8d865bd0f69168fed76711f065a898a6015c6/docs/design/trust-chain.md#L225-L238
[route-compose]: https://github.com/0gfoundation/0g-pc-e2ee/blob/5de8d865bd0f69168fed76711f065a898a6015c6/client/route/route.go#L1122-L1220
[baseline]: https://github.com/0gfoundation/0g-pc-e2ee/blob/4208145aaae06541568251f3f65b8987a10fe84f/client/evidence/brokercompose.json
[application-gap]: https://github.com/0gfoundation/0g-pc-e2ee/blob/4208145aaae06541568251f3f65b8987a10fe84f/docs/design/trust-chain.md#L332-L387
[release-compose]: https://github.com/0gfoundation/0g-pc-e2ee/releases/download/release-2026.09.28.1/docker-compose.release.yml
[sealing-policy]: https://github.com/0gfoundation/0g-pc-e2ee/blob/5de8d865bd0f69168fed76711f065a898a6015c6/client/cmd/gateway/sealmodels.go#L211-L242
[privacy-mode]: https://docs.0g.ai/developer-hub/building-on-0g/compute-network/router/privacy
[response-auth]: https://github.com/0gfoundation/0g-pc-e2ee/blob/5de8d865bd0f69168fed76711f065a898a6015c6/client/core/resmeta.go#L28-L43
[stream]: https://github.com/0gfoundation/0g-pc-e2ee/blob/5de8d865bd0f69168fed76711f065a898a6015c6/client/core/stream.go#L180-L320
