# ACI — Attested Confidential Inference

An interoperable interface for AI inference services that prove what
workload is serving the API and bind every response back to it: TEE
attestation, per-request signed receipts, end-to-end encryption to attested
keys, and verified aggregation.

| Document | Contents |
| --- | --- |
| [aci.md](aci.md) | The specification (`aci/1`, draft) |
| [test-vectors.md](test-vectors.md) | Byte-exact vectors for every digest, canonicalization, and signature construction |
| [related-work.md](related-work.md) | Positioning against other confidential-inference systems and standards |

Start with [aci.md](aci.md) §1 for the trust model and the conformance
summary. Implementers should validate against the test vectors first —
canonicalization is where independent implementations diverge.

This repository is the reference implementation. Known gaps between it and
the spec are tracked in
[docs/reviews/aci-spec-conformance-gaps.md](../docs/reviews/aci-spec-conformance-gaps.md).
Licensed under Apache-2.0 (see [LICENSE](../LICENSE)).
