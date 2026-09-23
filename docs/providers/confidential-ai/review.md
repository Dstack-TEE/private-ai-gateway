# Confidential AI Review

Date: 2026-09-23 UTC.
Operator: Inexorable, Inc. d/b/a Confidential AI.
Production endpoint: `https://api.confidential.ai`.
Candidate endpoint: `https://candidate.api.confidential.ai`.

Source repos reviewed:

- `confidential-dot-ai/confidential-inference` at `main`
  `475c35da81d8684fa1a38c9306774e203f0465cd` and the signed release tag
  `candidate-v7` `810330265402575c0651476202b4d2af21ba385f`
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

Confidential AI is **not acceptable** today.

- The production endpoint serves no inference. On 2026-09-23 `/health`
  reported `inferenceAvailable: false` and `/attestation` returned an
  `outage-status` statement from a maintenance gateway.
- The production release bundles on `main` and at `candidate-v7`
  (`releases/production/release-bundle.json`,
  `releases/conf-inference-prod/release-bundle.json`) configure
  `--front-door-mode=webpki`. In that mode the gateway reports
  `tls.binding.status: "not-proven"` because the public TLS key is supplied
  through Kubernetes and is visible to the host
  (`services/gateway/src/attestation.rs`, `tls_binding`). That is a hard reject:
  no enforceable channel binding on the request path.
- No signed production release exists. Only `candidate-v*` and `staging-v*`
  tags carry Sigstore release bundles, so production measurements cannot be
  reviewed before rollout (criterion 7).
- The provider's own threat model (`docs/threat-model.md`, "The c8s operator
  key" and "The control-plane state disk") states that a cluster-admin
  credential can exec into a pod inside the TEE and read workload memory. Two
  paths lead to that credential: the operator key can request a
  `system:masters` kubeconfig, and the host can read the unencrypted RKE2 state
  disk that holds the RKE2 client CA key. That is a hard reject: plaintext user
  content can leave the accepted trust boundary. Both fixes (removing the
  operator key and pinning RTMR3 to zero, and encrypting the state disk) are
  documented as planned.

The candidate environment is materially closer. It runs `candidate-v7` with
`tls.mode: "acme"`: an admitted `c8s acme` sidecar generates the serving key
inside the TEE and keeps it on a Memory-medium `emptyDir`
(`c8s/internal/cmds/acme/cmd.go`). That resolves the channel-binding
failure for the candidate shape, but not the cluster-admin paths above. The
provider becomes a candidate for **acceptable with conditions** only after
production uses a TEE-held front door, publishes signed releases, removes the
operator key with RTMR3 pinned to zero, and encrypts the control-plane state
disk.

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

- **Front-door binding exists in the candidate shape.** A live probe of
  `candidate.api.confidential.ai` on 2026-09-23 returned an attest-lb receipt
  whose nonce equalled the client nonce and whose `serving_leaf_sha256` equalled
  the SHA-256 of the leaf observed on the same connection. The quote's
  `report_data[0:48]` equalled the recomputed transcript, with zero padding.
  The probe did not run DCAP collateral verification.
- **Signed candidate releases.** `candidate-v7` publishes `release-bundle.json`,
  `release-bundle.sigstore.json`, and `release-tag-commit.txt`. The bundle pins
  node image digest, workload image digests and argv, allowlist digest, c8s
  commit, operator key, and model revision with a dm-verity root.
- **In-TEE TLS termination.** The c8s router terminates public TLS inside the
  CVM and forwards to the provider gateway, which reaches the sglang router and
  inference workers over the RA-TLS mesh.

Failed or not yet evidenced:

- **Channel binding in production (criterion 2).** Production is still
  `webpki`. The attest-pq over-encryption tunnel is hardware-bound, but it does
  not stream and caps responses at 32 MiB, so it cannot carry chat traffic.
- **Release publication (criteria 7 and 13).** There is no signed production
  release. Seven candidate releases were published between 2026-09-21 and
  2026-09-23, so a publication and notice window must be agreed before strict
  pins are practical.
- **GPU evidence.** `candidate-v7` returns `gpuEvidence.status:
  "not-exposed-by-c8s"`. GPU CC is checked by a boot gate inside the measured
  node image, not by evidence the relying party can verify. The gateway would
  record `gpu_attested` as unknown.
- **Cluster-admin reaches TEE memory (hard reject).** Per the provider's threat
  model, even in static policy mode the operator key can write CDS secrets and
  request a `system:masters` kubeconfig, and the Kubernetes control plane is
  outside the trust boundary, so a cluster-admin can exec into TEE pods. The
  host can also mint cluster-admin from the RKE2 client CA key on the
  unencrypted state disk. Required fix: remove the operator key with RTMR3
  pinned to zero, and encrypt the state disk.
- **Allowlist breadth.** The live candidate allowlist admits several
  infrastructure images (`c8s-operator`, `cds`, `nginx-unprivileged`,
  `nri-image-policy`, `ratls-mesh`, `volumed`) with `any` command and argument
  policy, plus a Tailscale control-plane workload. The review must confirm none
  of them can obtain the serving key, a mesh identity for the inference path, or
  plaintext request bodies.
- **Attestation scope.** Responses declare `scope:
  "launch-or-admission-only"` and `operationalStatus: "not-verified"`. Mesh
  peers are authenticated by CA chain, not per-peer measurement, and the mesh CA
  rotates when CDS restarts.
- **Model identity.** `/v1/models` has listed a model the gateway does not
  serve, and the documented model ID differs from the served ID. The adapter
  must bind the exact served ID.
- **Receipts.** The provider emits no per-response signature.

## Adapter Requirements

P0:

- Accept only `tls.mode` values that hold the key inside the TEE (`acme`,
  `tee-webpki`, `cds`), and require a verified attest-lb front-door receipt.
  Fail closed on `webpki`.
- Recompute the attest-lb transcript from the observed leaf and emit a
  `tls_spki_sha256` binding for the origin.
- Pin MRTD, RTMR1, RTMR2, and the allowlist digest from a reviewed,
  Sigstore-verified release bundle, and require RTMR3 to be zero. Do not trust bundle values
  fetched at verification time.
- Require DCAP `UpToDate` and reject debug TDs.
- Implement the transcript from the specification and the MIT clients. Do not
  copy AGPL-3.0 c8s code into the gateway.

P1:

- Record `release.id`, `bundleSha256`, `meshCaSha256`, and the front-door mode
  in `claims.extra`.
- Add negative tests for a swapped serving leaf, a wrong nonce, an unpinned
  measurement, a `webpki` mode, and a rotated mesh CA.
- Handle mesh CA rotation as a lease-invalidating event.

## Open Questions For The Provider

1. When will production use a TEE-held front door (`acme` or `tee-webpki`)?
2. Will production releases be signed and published before rollout, with
   notice for emergency changes?
3. When will the operator key be removed (RTMR3 pinned to zero) and the
   control-plane state disk encrypted?
4. Can nonce-bound NVIDIA evidence be exposed to relying parties?
5. How are mesh CA rotations published?
6. Which production model IDs are stable?
