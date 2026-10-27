# Confidential AI Review

Date: 2026-10-27 PDT (round 3).

> [!NOTE]
> This is a dated admission record; see [Documentation conventions](../../README.md#documentation-conventions).
> Currency check, 2026-10-27 PDT: [round 3](#round-3-2026-10-27-pdt) reviews
> deployed Confidential Inference `v0.15.15`, which pins c8s `v0.59.1`
> (`4c916ece`). c8s `v0.74.1` was also read but is not deployed. This corrects
> the 2026-10-26 currency note: the deployed c8s version is `v0.59.1`, not
> `v0.74.1`. The [round-2 record](#round-2-2026-10-15-pdt) is retained below.

Operator: Inexorable, Inc. d/b/a Confidential AI. Product: Confidential Inference.
Endpoint: `https://api.confidential.ai`.

## Round 3 (2026-10-27 PDT)

Reviewed Confidential Inference [`v0.15.15`](https://github.com/confidential-dot-ai/confidential-inference/releases/tag/v0.15.15)
(`f555f44f01be5940cad99c81c7c18ecb4edb4c59`, deployed), which pins c8s
[`v0.59.1`](https://github.com/confidential-dot-ai/C8s/tree/4c916ecea7221e506988ff9e37725a314a64b562)
(`4c916ece`). Also read c8s [`v0.74.1`](https://github.com/confidential-dot-ai/C8s/releases/tag/v0.74.1)
(`8349b2d0`, including the per-pod mesh, #809), `v0.15.18-staging`
(`b557082`), and confidential-inference `main` (`6540905`). None of these
newer versions is deployed. The newer Confidential Inference versions also
pin c8s `v0.59.1`.

### Verdict

Still **not acceptable**. Both round-2 hard rejects remain in the deployed
release and in c8s `v0.74.1`.

### Live Check (2026-10-27 00:12–00:17 PDT)

- `/attestation` reports `v0.15.15` and bundle `sha256:3c875412…`, with
  `releaseSelection.status: matched`. The raw `/allowlist` bytes hash to
  `8de45cdb…eb58d`, which equals the signed release `allowlist.json`.
- TEErminator `v0.0.4` returned `Verified` for `attest-lb`. It pinned
  MRTD/RTMR1/RTMR2 from the bundle, the exact allowlist bytes, workload
  `c8s-router-fb21f5cac23c`, online Intel `UpToDate` only, the operator-signed
  route manifest, and the RTMR3 operator-key seed. A zero RTMR3 pin failed.
- RTMR3 equals SHA-384(0⁴⁸ ‖ SHA-384(PEM)) of the live operator key. That key
  is published only in `docs/operator-key.pem` on `main`, not in the signed
  release.
- TEErminator `v0.0.2`, which the provider's `docs/verification.md` names,
  now fails with a REPORT_DATA mismatch.
- The Sigstore bundle digest and certificate identity match the tag workflow.
  The signature and Rekor inclusion were not verified locally.

### Resolved or Improved

- `nginx-unprivileged` now has exact args, env, and mounts.
- Workers additionally require the label `confidential.ai/traffic-eligible=true`.
  This is still label-based admission.

### Open Findings

1. **Operator data-plane authority (hard reject).**
   - The RBAC is unchanged (`cred-release-rbac.yaml:25-42` @ `v0.59.1`;
     `operator-scope-policy.yaml:50` exempts only the system namespaces, and
     `v0.74.1` adds only `c8s-router`).
   - The live `busybox` entry pins only `/bin/sh /script/setup|teardown`,
     with `any` args, env, and mounts, and c8s allowlists have no namespace
     scope. A ConfigMap mounted at `/script` therefore runs arbitrary shell
     in `confidential-inference`.
   - With the two worker labels, such a pod receives prompts from
     `sglang-router` (`release/values.yaml:519-521`) and can log them for
     `pods/log`.
   - The gateway still accepts any `router-*` Service
     (`services/gateway/src/runtime_settings.rs:116`), and
     `namedWorkloadProxy` is off in production (`release/values.yaml:57-58`).
   - The per-pod mesh in `v0.74.1` authenticates mesh membership, not
     workloads, except at the front-door routing sidecar (`docs/armtls.md`).
     It does not close this path. The unreleased `--worker-urls` change still
     resolves through operator-writable Services.
2. **Host server enrollment (hard reject).** Unchanged
   (`docs/operator.md:265-283` @ `v0.59.1`, `:300-318` @ `v0.74.1`).
3. **RTMR3 not in the signed release.** Live RTMR3 matches a key published
   on `main` only. The release manifest still states it "does not require a
   fixed … operator key".
4. **Defense in depth.** The #608 sandbox is still `audit`
   (`image-policy.yaml.in:234` @ `v0.59.1`, `:655` @ `v0.74.1`).
5. **Stale client docs.** The documented TEErminator `v0.0.2` no longer
   verifies production.

### Requests To The Provider (in addition to round 2)

1. Remove the `any` mounts and args from `busybox`, or restrict it to
   `local-path-storage`.
2. Admit workers only by attested workload identity. For example, have the
   router dial workers through a peer-workload-checked mesh hop, and enable
   `namedWorkloadProxy` in production.
3. Put the operator public key or the expected RTMR3 in the signed release
   bundle.
4. Update `docs/verification.md` to a TEErminator version that verifies the
   current transcript.

### Not Verified

- Neither hard reject was exploited; both findings are from source review.
- The Sigstore signature and Rekor inclusion were not checked locally.
- Per-pod mesh code was reviewed only through documentation and the PR.
- Model selection, request fidelity, and receipts were not re-examined beyond
  round 2. Why discovery advertises `candidate.api.confidential.ai` on the
  production origin was not established.

## Round 2 (2026-10-15 PDT)

Sources reviewed:

- [`confidential-dot-ai/confidential-inference`](https://github.com/confidential-dot-ai/confidential-inference/tree/b5973fffd52d5072065eb711cd707cc18f67ba8a) at
  `b5973fffd52d5072065eb711cd707cc18f67ba8a` (`v0.14.29`, deployed), with the
  threat model at [`cedbe87fc6bc1e1d919ba1fc300b15f81b2300a4`](https://github.com/confidential-dot-ai/confidential-inference/blob/cedbe87fc6bc1e1d919ba1fc300b15f81b2300a4/docs/threat-model.md)
- [`confidential-dot-ai/c8s`](https://github.com/confidential-dot-ai/C8s/tree/deca64d1f5bf6c70e00c1a42ad0638bdf654efbc) at `deca64d1f5bf6c70e00c1a42ad0638bdf654efbc`
  (v0.44.6, deployed) and `f0390b28767c0964766ae86c3896ce22bc73dcdf` (HEAD);
  AGPL-3.0, read for the protocol only
- [`confidential-dot-ai/TEErminator`](https://github.com/confidential-dot-ai/TEErminator/tree/6184215d76904db37efe862670305ffeb494ffb3) at `6184215d76904db37efe862670305ffeb494ffb3`
  and [`confidential-dot-ai/c8s-verify-js`](https://github.com/confidential-dot-ai/c8s-verify-js/tree/5ed071789114254af9ba20a8a25269ed5c28e004) at
  `5ed071789114254af9ba20a8a25269ed5c28e004` (both MIT)

> **Gateway verification:** a `c8s` adapter exists for the schema v2
> attestation. It must be rewritten for schema v3 before use. Having an adapter
> does not change this admission verdict.

### Verdict

Confidential AI is **not acceptable**. The provider's operator role can still
route plaintext to code it chooses, and read that code's output, without
changing anything a relying party pins. The host can also enroll a Kubernetes
server. Either path lets plaintext leave the accepted trust boundary.

### Resolved since round 1 (2026-09-24)

- cred-release issues `c8s:node-operators` rather than `system:masters`.
  Measured admission guards deny exec, attach, port-forward, and ephemeral
  containers.
- RKE2 state lives on per-boot plain dm-crypt scratch, and `scratch-enforce`
  must succeed before RKE2 starts. There is no integrity protection, which does
  not affect confidentiality.
- `gateway`, `sglang-router`, and `inference-worker-*` have exact command, args,
  env, and mounts. The privileged state mounter is gone.
- The front door holds its TLS key inside the TEE. A live `attest-lb` check
  passed: nonce echo, TLS leaf, quote signature chain, identity proof,
  MRTD/RTMR1/RTMR2 against the release bundle, and the allowlist stamp
  (`cf40699a…`).

### Attestation Contract (schema v3)

- `/attestation` is metadata. It does not bind a client nonce, and it carries no
  per-workload receipts.
- The connection proof is `GET /.well-known/c8s/attest-lb?nonce=<b64url32>`. Its
  `report_data` is SHA-384 over length-prefixed fields: `"c8s/attest-lb/v1"`,
  the mode, the nonce, and the SHA-256 of the serving leaf, the mesh leaf, and
  the mesh CA. The serving leaf is the one observed on the same connection.
- The allowlist digest travels in a mesh-CA-signed matched-workload stamp (OID
  `1.3.6.1.4.1.66378.1.5`) on the front-door mesh leaf. It is vouched for by the
  CA, not by hardware.
- Only the front-door node is attested to the relying party. The gateway,
  router, and workers are trusted only transitively, and the mesh authenticates
  nodes, not pods (c8s `cmd/armtls-mesh/DESIGN.md`). PRs that would check
  upstream workload stamps (#703, #750) were closed unmerged.
- The provider's documented client flow does not pin RTMR3, which is the
  operator-key seed (`docs/verification.md:161-167`).

### Open Findings

1. **Operator data-plane authority (hard reject).**
   - `c8s-node-operator` has full CRUD on pods, `pods/log`, Services, Endpoints,
     ConfigMaps, Secrets, NetworkPolicies, and workloads in non-system
     namespaces, including `confidential-inference`
     (`cred-release-rbac.yaml:25-57`, `operator-scope-policy.yaml:50-60`).
   - **Fake worker.** `sglang-router` admits any Ready pod labelled
     `app.kubernetes.io/component=inference-worker`. Allowlist entries such as
     `nginx-unprivileged` have `any` args, env, and mounts and are not
     namespace-scoped. A ConfigMap-configured nginx pod with that label can
     receive prompts and log request bodies.
   - **Exfiltration.** Those logs are readable through `pods/log`, and the log
     collector ships them to `admin.api.confidential.ai`, outside the TEE
     (`release/values.yaml:853`).
   - **Redirect.** The gateway's runtime `routerService` accepts any `router-*`
     Service (`services/gateway/src/runtime_settings.rs:115-122`).
   - None of this changes the allowlist digest.
2. **Host server enrollment (hard reject).** Signed but unencrypted launch media
   exposes the RKE2 join tokens to the infrastructure operator, and port 9345
   is outside the mesh (c8s `docs/operator.md:269-283`). A server join returns
   bootstrap data that includes the client CA key. That is enough to mint
   `system:masters`. This is from source review; it was not exploited.
3. **RTMR3 not pinned by the documented flow.** A cluster with the same image
   and allowlist but a different operator key would pass. The gateway must pin
   it.
4. **Defense in depth.** The #608 host-privilege sandbox is `audit`, not
   `enforce` (`image-policy.yaml.in:231`).

### Adapter Requirements (schema v3)

- Obtain a fresh `attest-lb` proof per lease. Bind the nonce, the mode, and the
  serving leaf observed on the same connection. Recompute `report_data`, verify
  the identity proof, and check the mesh chain.
- Require DCAP with online collateral and `UpToDate`, and reject debug TDs.
- Pin MRTD, RTMR1, RTMR2, and **RTMR3** from a reviewed, Sigstore-verified
  release.
- Pin the raw `/allowlist` bytes digest and require the `c8s-router` stamp to
  match it. Review every allowlist entry.
- Treat `/attestation` as metadata only.
- Implement the transcripts from specifications and MIT references. Do not copy
  AGPL-3.0 c8s code.

### Requests To The Provider

1. Remove the operator role's data-plane authority in the inference namespace.
   Alternatively, admit workers only by attested workload identity, and remove
   or namespace-restrict the `any` entries.
   *Done when:* the released operator credential is denied creating a
   worker-set pod, editing routing Services or ConfigMaps, and reading
   `pods/log` in that namespace.
2. Keep server tokens out of host-readable launch media, or put 9345 behind
   attested mTLS, and rotate the tokens.
   *Done when:* the ISO contents plus network access cannot complete a server
   join.
3. Publish the expected RTMR3 or the operator public key in each signed release.
4. Run the #608 sandbox in `enforce` mode.
