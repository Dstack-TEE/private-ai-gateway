# ACI — Attested Confidential Inference

An interoperable interface for AI inference services that prove what
workload is serving the API and bind every response back to it: TEE
attestation, per-request signed receipts, end-to-end encryption to attested
keys, and verified aggregation.

| Document | Contents |
| --- | --- |
| [aci.md](aci.md) | The specification (`aci/1`, draft) |
| [test-vectors.md](test-vectors.md) | Byte-exact vectors for every digest, signature, and sealing construction |
| [related-work.md](related-work.md) | Positioning against other confidential-inference systems and standards |
| [../docs/quickstart.md](../docs/quickstart.md) | Hands-on guide: verify a live deployment yourself |

New to ACI? Run the [quickstart](../docs/quickstart.md) first. Then read
[aci.md](aci.md) §1 with the §3 trust-chain diagram next to it.
Implementers: validate against the test vectors early. The byte templates
and served-bytes bindings are where independent implementations diverge.

By task:

| To do this | Read |
| --- | --- |
| Verify a live deployment right now | [quickstart](../docs/quickstart.md) |
| Get the trust model and conformance rules | [aci.md](aci.md) §1 |
| Implement identity and the workload keyset | [aci.md](aci.md) §3 |
| Parse the attestation report and evidence | [aci.md](aci.md) §4 |
| Encrypt request and response bodies end to end | [aci.md](aci.md) §6 |
| Produce or verify receipts | [aci.md](aci.md) §7 |
| Build or audit an aggregator | [aci.md](aci.md) §1.2, §5.3, §7.5, §8 |
| Implement a verifier | [aci.md](aci.md) §9, then [test-vectors.md](test-vectors.md) |
| Audit the upstreams behind an aggregator deployment | [provider verification notes](../docs/providers/README.md) |
| Compare ACI to other systems | [related-work.md](related-work.md) |
| Run or audit the reference implementation | [../README.md](../README.md), [known gaps](../docs/reviews/aci-spec-conformance-gaps.md) |

This repository is the reference implementation. Known gaps between it and
the spec are tracked in
[docs/reviews/aci-spec-conformance-gaps.md](../docs/reviews/aci-spec-conformance-gaps.md).
Licensed under Apache-2.0 (see [LICENSE](../LICENSE)).
