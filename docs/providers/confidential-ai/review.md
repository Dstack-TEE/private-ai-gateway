# Confidential AI Review

Date: 2026-09-23 UTC (live endpoints rechecked at 20:08 UTC).
Operator: Inexorable, Inc. d/b/a Confidential AI.
Production endpoint: `https://api.confidential.ai`.
Candidate endpoint: `https://candidate.api.confidential.ai`.

Source repos reviewed:

- `confidential-dot-ai/confidential-inference` at `main`
  `a54319a2ebb2ae51f161d7c2085bffcca02e082c`, which is also the signed
  pre-release tag `v0.13.28-rc.2`
- `confidential-dot-ai/c8s` at `ec67bd84615990d797ae687f449120915e5002ea`
  (AGPL-3.0; read for the protocol, not a source for adapter code)
- `confidential-dot-ai/TEErminator` at
  `8d302348a7f66a2bfd74b24535c6f2f791b2839e` (MIT)

Supporting documentation:

- `confidential.ai/docs/inference-api/attestation`
- `confidential.ai/docs/c8s`
- `confidential.ai/docs/attested-builds`

> **Gateway verification:** not implemented. This review is the admissions
> audit; there is no `verification.md` until an adapter lands.

## Verdict

Confidential AI is **not acceptable** today. One hard reject remains, plus one
release-process gap.

- **Cluster-admin reaches TEE memory (hard reject).** The provider's threat
  model (`docs/threat-model.md`, "The c8s operator key" and "The control-plane
  state disk") states that a cluster-admin credential can exec into a pod
  inside the TEE and read workload memory. Two paths lead to that credential:
  the operator key can request a `system:masters` kubeconfig even in static
  policy mode, and the host can read the unencrypted RKE2 state disk that holds
  the RKE2 client CA key. Plaintext user content can therefore leave the
  accepted trust boundary. The provider documents both fixes as planned:
  removing the operator key with RTMR3 pinned to zero, and encrypting the state
  disk. On 2026-09-23 RTMR3 was non-zero on both production and candidate.
- **Production runs an unsigned release (criterion 7).** Production serves
  `v0.13.27-static-attestation-20260910`, which has no signed GitHub release.
  The signed pre-release `v0.13.28-rc.2` runs on candidate, and the provider
  has just allowed the production hostname on the candidate router, so a
  production cutover to signed releases appears imminent.

Resolved on 2026-09-23:

- Production serves inference again. Earlier the same day it was in maintenance
  mode.
- Production and candidate both use `tls.mode: "acme"`. An admitted `c8s acme`
  sidecar generates the serving key inside the TEE and keeps it on a
  Memory-medium `emptyDir` (`c8s/internal/cmds/acme/cmd.go`). The live
  attest-lb binding holds on both endpoints (see Criteria Status).

The provider becomes a candidate for **acceptable with conditions** once
production runs a signed release, the operator key is removed with RTMR3
pinned to zero, and the control-plane state disk is encrypted.

## Trust Model If Admitted

1. Private AI Gateway fetches `GET /attestation` with a fresh 32-byte nonce,
   encoded as unpadded base64url.
2. It verifies the TDX quote in the `frontDoor.receipt` (`c8s/attest-lb/v1`)
   with DCAP and requires `UpToDate`, and rejects debug TDs.
3. It recomputes the attest-lb transcript from the TLS leaf it observed on its
   own connection:

   ```text
   SHA-384( LP("c8s/attest-lb/v1") || LP(mode) || LP(nonce) ||
            LP(SHA-256(serving_leaf_DER)) || LP(SHA-256(mesh_leaf_DER)) ||
            LP(SHA-256(mesh_CA_DER)) )
   ```

   `LP` is a 4-byte big-endian length prefix. The 48-byte digest must equal
   `report_data[0:48]`, and `report_data[48:64]` must be zero.
4. It matches MRTD, RTMR1, and RTMR2 against a reviewed, signed release bundle,
   and requires RTMR3 to be zero (no operator credential-release path armed at
   boot).
5. It checks the per-workload receipts, the `identity_proof` signatures, the
   mesh CA, and the admitted image digests and argv against the same bundle.
6. It pins the serving leaf SPKI for the lease. The serving key is TEE-held, so
   the SPKI pin is equivalent to the leaf binding. Scope is per-router.

## Criteria Status

Passed:

- **Front-door channel binding (criterion 2).** Live probes of both endpoints
  at 20:08 UTC returned an attest-lb receipt whose nonce equalled the client
  nonce and whose `serving_leaf_sha256` equalled the SHA-256 of the leaf
  observed on the same connection. Each quote's `report_data[0:48]` equalled
  the recomputed transcript, with zero padding, and the TD attributes had the
  debug bit clear. The probes did not run DCAP collateral verification.
- **Production allowlist.** Production runs static policy mode. Its active
  allowlist admits eight workloads (`c8s-tls-lb`, gateway, state mounter, two
  inference workers, sglang router, and two metrics workloads), none with an
  `any` command or argument policy.
- **Signed releases exist.** `v0.13.28-rc.2` publishes `release-bundle.json`,
  `release-bundle.sigstore.json`, and `release-tag-commit.txt`. The bundle pins
  node image digest, workload image digests and argv, allowlist digest, c8s
  commit, operator key, and model revision with a dm-verity root.
- **In-TEE TLS termination.** The c8s router terminates public TLS inside the
  CVM and forwards to the provider gateway, which reaches the sglang router and
  inference workers over the RA-TLS mesh.

Failed or not yet evidenced:

- **Cluster-admin reaches TEE memory (hard reject).** See Verdict. Required
  fix: remove the operator key with RTMR3 pinned to zero, and encrypt the
  state disk.
- **Release publication (criteria 7 and 13).** Production runs an unsigned
  release. The provider published two release candidates on 2026-09-23 and
  seven candidate tags in the two days before, so a publication and notice
  window must be agreed before strict pins are practical.
- **GPU evidence.** Production copies two raw NVIDIA evidence items from worker
  receipts (`raw-receipt-evidence`) and states that cryptographic GPU
  verification remains a c8s verifier dependency. Candidate returns
  `not-exposed-by-c8s` and relies on a boot gate inside the measured node
  image. Unless raw evidence is verified with NRAS against the CPU transcript
  nonce, the gateway records `gpu_attested` as unknown.
- **Candidate allowlist breadth.** The candidate allowlist admits several
  infrastructure images (`c8s-operator`, `cds`, `nginx-unprivileged`,
  `nri-image-policy`, `ratls-mesh`, `volumed`) with `any` command and argument
  policy, plus a Tailscale control-plane workload. If that shape reaches
  production, the review must confirm none of them can obtain the serving key,
  a mesh identity for the inference path, or plaintext request bodies.
- **Attestation scope.** Responses declare `scope:
  "launch-or-admission-only"` and `operationalStatus: "not-verified"`. Mesh
  peers are authenticated by CA chain, not per-peer measurement, and the mesh CA
  rotates when CDS restarts.
- **Model identity.** Production `/v1/models` lists
  `MiniMaxAI/MiniMax-M3-MXFP8` alongside the served
  `deepseek-ai/DeepSeek-V4-Flash-0731`, and the documented model ID differs from
  the served ID. The adapter must bind the exact served ID.
- **Receipts.** The provider emits no per-response signature.

## Adapter Requirements

P0:

- Accept only `tls.mode` values that hold the key inside the TEE (`acme`,
  `tee-webpki`, `cds`), and require a verified attest-lb front-door receipt.
  Fail closed on `webpki`.
- Recompute the attest-lb transcript from the observed leaf and emit a
  `tls_spki_sha256` binding for the origin.
- Pin MRTD, RTMR1, RTMR2, and the allowlist digest from a reviewed,
  Sigstore-verified release bundle, and require RTMR3 to be zero. Do not trust
  bundle values fetched at verification time.
- Require DCAP `UpToDate` and reject debug TDs.
- Implement the transcript from the specification and the MIT clients. Do not
  copy AGPL-3.0 c8s code into the gateway.

P1:

- Record `release.id`, `bundleSha256`, `meshCaSha256`, and the front-door mode
  in `claims.extra`.
- Add negative tests for a swapped serving leaf, a wrong nonce, an unpinned
  measurement, a non-zero RTMR3, a `webpki` mode, and a rotated mesh CA.
- Handle mesh CA rotation as a lease-invalidating event.

## Open Questions For The Provider

1. When will the operator key be removed (RTMR3 pinned to zero) and the
   control-plane state disk encrypted?
2. When will production run signed releases, and what notice will precede
   production measurement changes, including emergency changes?
3. Can nonce-bound NVIDIA evidence be verified by relying parties?
4. How are mesh CA rotations published?
5. Which production model IDs are stable?
