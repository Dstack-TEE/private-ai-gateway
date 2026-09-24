# Confidential AI (c8s) Verification

This reference is for Private AI Gateway operators who configure Confidential
AI (`https://api.confidential.ai`, operated by Inexorable, Inc.). Confidential
AI runs on c8s (Confidential Kubernetes). Each node is an Intel TDX CVM. The
`c8s-tls-lb` front door terminates public TLS inside that CVM. Request content
is then processed by workloads on other nodes. The gateway verifies the front
door, every workload on the plaintext path, and each of their node measurements.
It then pins the serving TLS key for forwarded traffic.

The admission review is `docs/providers/confidential-ai/review.md` (on the
review branch). That review lists a hard reject that is still open: the c8s
operator key can reach cluster-admin. This adapter does not resolve it. It
records the key state honestly (see [RTMR3 and the operator key](#rtmr3-and-the-operator-key)).
A second open finding is that admission leaves the plaintext-path workloads'
environment and mounts unconstrained. The adapter reports this and refutes
`serving_software_known_good` (see
[Admission env and mounts](#admission-env-and-mounts)).

## Configure a Confidential AI origin

```json
[
  {
    "name": "confidential-ai",
    "provider": "c8s",
    "base_url": "https://api.confidential.ai",
    "models": {
      "confidential-ai/deepseek-v4-flash": "deepseek-ai/DeepSeek-V4-Flash-0731"
    },
    "bearer_token": "<confidential-ai-api-key>"
  }
]
```

`base_url` must be a root HTTPS origin with no userinfo, path, query, or
fragment. The entry has no measurement fields. The only trust input is the
reviewed registry `scripts/provider_verifier/provider_refs/c8s.json`, which
ships with the gateway release. `/attestation` needs no credential.
`bearer_token` is used only for model traffic. Map public models to the exact
served model ID, because the provider catalog lists models that it does not
serve.

## Multi-node topology

On 2026-09-24 neither deployment ran every plaintext-handling workload on the
front-door node. Each `/attestation` response carries one nonce-bound receipt
per workload in `receipts[]` (`{target, workload, identity, admittedLaunch,
receipt}`). The quotes group into two nodes per deployment by RTMR0, the
per-host firmware configuration. MRTD, RTMR1, RTMR2, and RTMR3 are identical on
every node of a deployment.

| Deployment | Node (RTMR0 prefix, FMSPC) | Workloads |
| --- | --- | --- |
| production | `8cd47f5d…`, `00A06D080000` | front door, `gateway`, `metrics-collector` |
| production | `e6351ce4…`, `00A06D080000` | `inference-worker-0`, `inference-worker-1`, `sglang-router`, `kube-state-metrics` |
| candidate | `41aa2900…`, `B0C06F000000` | front door, `gateway`, `metrics-collector`, `kube-state-metrics` |
| candidate | `d695907c…`, `00A06D080000` | `inference-worker-0`, `inference-worker-1`, `sglang-router` |

A request travels from the front door to the provider `gateway`, then to
`sglang-router`, then to an `inference-worker-*`. All of these see request
plaintext. The front-door quote alone says nothing about the worker nodes, so
the adapter requires a verified receipt for each of these workloads.
`metrics-collector` and `kube-state-metrics` are not on the request path and are
not required.

## Verification algorithm

The bridge reports verifier ID `private-ai-verifier/c8s/v1`. For each
verification, it:

1. Generates a fresh 32-byte nonce and encodes it as unpadded base64url
   (43 characters). The provider rejects 64-hex nonces and accepts each nonce
   only once.
2. Opens a TLS connection to the origin with WebPKI validation. It records the
   serving leaf certificate of that connection. On the same connection it
   sends `GET /attestation?nonce=<nonce>` and reads at most 8 MiB of
   `application/json`.
3. Requires `schemaVersion` 2 and a top-level `nonce` equal to the client
   nonce.
4. Looks up `release.id` in the registry. Unknown releases fail. The response
   `release.bundleSha256` must equal the reviewed bundle digest.
5. Requires `tls.mode` to be `acme`, `tee-webpki`, or `cds`. In these modes the
   TEE holds the serving key. `webpki` and every other mode fail.

### Front door

6. Requires `frontDoor.source` to be `c8s-tls-lb` and requires a receipt with
   version `c8s/attest-lb/v1` and platform `tdx`. The receipt `nonce` must
   equal the client nonce. The signed `front_door_mode` must be a TEE-held mode
   equal to `tls.mode`. `serving_leaf_sha256` must name the leaf observed in
   step 2.
7. Parses `cds_cert_pem` as exactly one mesh leaf and one mesh CA. The CA must
   be a self-signed CA certificate, and the leaf must be issued by it. Both
   must be inside their validity period. `c8s.meshCaSha256` must equal the
   SHA-256 of the mesh CA DER. If the registry lists accepted mesh CAs for the
   release, the CA must be one of them. This CA is the mesh CA for every
   workload check below.
8. Recomputes the attest-lb transcript digest (see
   [Transcript constructions](#transcript-constructions)) from the client nonce
   and the observed serving leaf.
9. Verifies `identity_proof`. The algorithm must be `ecdsa-sha384`.
   `leaf_sha256` and `mesh_ca_sha256` must name the returned certificates. The
   signature must verify under the mesh leaf key, using ECDSA with SHA-384 over
   the 48-byte transcript digest.
10. Requires `c8s.activeAllowlist.sha256` to be in the release's accepted set.
    Recomputes the canonical digest of `c8s.activeAllowlist.document` (see
    [Allowlist canonicalization](#allowlist-canonicalization)) and requires it
    to equal `c8s.activeAllowlist.sha256`.
11. Decodes the front-door quote as hex or base64 (standard or URL-safe). It
    must be a TDX v4 quote.

### Plaintext-path workloads

12. If the response states `c8s.attestationProtocol`, it must equal the
    release's reviewed `workload_attestation_protocol`.
13. Requires exactly one `receipts[]` entry for each target in the release's
    `required_workloads`. A missing or duplicated required target fails. An
    `inference-worker-*` target that the release does not list also fails,
    because the router could send requests to it. Other targets are ignored.
14. For each required target, it:
    1. Requires the entry's `workload` and `identity` to equal the reviewed
       target binding.
    2. Requires receipt version `c8s/attest-pq/v1`, platform `tdx`, and a
       receipt `nonce` equal to the client nonce.
    3. Parses `cds_cert_pem` as one mesh leaf and one mesh CA. The CA must be
       byte-identical to the front door's mesh CA, and the leaf must be inside
       its validity period and be issued by that CA.
    4. Parses the mesh leaf's matched-workload stamp (OID
       `1.3.6.1.4.1.66378.1.5`, strict minimal DER, exactly one). The stamped
       name must equal the reviewed `identity`. The stamped allowlist digest
       must equal `c8s.activeAllowlist.sha256`, so the document inspected in
       step 15 is the one that the workload was admitted under.
    5. Recomputes the attest-pq transcript for the release's protocol. Session
       fields must have their exact sizes.
    6. Verifies `identity_proof` as in step 9, under the workload's mesh leaf.
    7. Decodes the workload quote as in step 11.

15. Reads each required workload's allowlist entry (named by its stamped
    identity), plus each entry in the release's `additional_admission_entries`
    (`gateway-state-mounter`). An entry is pinned only if every init and main
    container has an `env` and a `mounts` policy of `exact` or `deny`. If the
    release sets `require_pinned_env_mounts` and any entry is not pinned,
    verification fails.

### Quotes and measurements

16. Verifies the front-door quote and every workload quote with `dcap_qvl`
    against Intel PCS collateral, concurrently (at most 4 at a time). Quotes
    with the same FMSPC and PCK CA share one collateral fetch.
17. For each verified quote, it:
    - requires TCB status `UpToDate`;
    - rejects a debug TD (any TUD bit in `td_attributes` byte 0);
    - requires `report_data[0:48]` to equal that quote's transcript digest and
      `report_data[48:64]` to be zero;
    - requires MRTD, RTMR1, and RTMR2 to equal the release pins, and RTMR3 to
      be in the release's accepted RTMR3 set.
18. Emits a `tls_spki_sha256` binding for the observed serving leaf. The
    TEE holds the key, so the SPKI pin is equivalent to the leaf binding. The
    Rust forwarding path enforces the pin on every model request.

Any failure fails the whole verification.

RTMR0 is not pinned. It carries the per-host virtual firmware configuration
and differs between nodes that run the same image. The adapter records each
workload's RTMR0 in `verified_workloads` so the node layout stays visible.

The scope is `router`. One origin and one SPKI serve every configured model.

## What each workload check binds

| Check | What it proves |
| --- | --- |
| DCAP `UpToDate`, debug bit clear | A genuine, patched, production TDX TD produced the quote. |
| MRTD, RTMR1, RTMR2 = release pins; RTMR3 accepted | The workload's node booted the reviewed c8s firmware, kernel, and rootfs, with a reviewed RTMR3 (operator-key state). |
| Receipt nonce = client nonce, and nonce in the transcript | The quote was produced for this verification. It is not a replay. |
| Transcript in `report_data[0:48]`, zero padding | The quote commits to this exact mesh leaf, mesh CA, session keys, `front_door_mode`, and nonce. |
| Identity proof | The TD that produced the quote holds the mesh leaf's private key. A copied public chain cannot sign a new transcript. |
| Mesh CA = front-door mesh CA | The workload holds a leaf from the same CDS mesh CA as the front door. It belongs to the same cluster, and it is the peer that the mesh authenticates on the request path. |
| `workload`/`identity` = target binding, and stamped name = `identity` | The reviewed workload identity is stamped by the mesh CA into a leaf that the quote binds. JSON labels cannot relabel one node's receipt as another's. |
| Stamped allowlist digest = active allowlist digest, which is accepted and whose document hashes to it | The CA admitted the workload under a reviewed allowlist, and that allowlist's document is the one the adapter inspects. |

## Transcript constructions

`LP(x)` is a 4-byte big-endian length followed by `x`. All hashes are over
DER-encoded certificates. The transcript digest fills `report_data[0:48]`, and
`report_data[48:64]` is zero. The mesh leaf signs the digest with
ECDSA-SHA384.

| Protocol | Used by | Transcript |
| --- | --- | --- |
| `c8s/attest-lb/v1` | Front door, both deployments | `SHA-384( LP("c8s/attest-lb/v1") ‖ LP(front_door_mode) ‖ LP(nonce32) ‖ LP(SHA-256(serving_leaf)) ‖ LP(SHA-256(mesh_leaf)) ‖ LP(SHA-256(mesh_CA)) )` |
| `c8s/attest-pq/v1` | Workloads on `v0.13.27-static-attestation-20260910` (production) | `SHA-384( LP("c8s-verify/v1") ‖ LP(front_door_mode) ‖ LP(SHA-256(mesh_CA)) ‖ LP(SHA-256(mesh_leaf)) ‖ LP(x25519_pub32) ‖ LP(mlkem768_ek1184) ‖ LP(nonce32) )` |
| `c8s/attest-pq/v1+xwing` | Workloads on `v0.13.28-rc.2` (candidate) | `SHA-384( LP("c8s-verify/v1") ‖ LP(front_door_mode) ‖ LP(SHA-256(mesh_CA)) ‖ LP(SHA-256(mesh_leaf)) ‖ LP(xwing_ek1216) ‖ LP(xwing_ct1120) ‖ LP(session_id16) ‖ LP(nonce32) )` |

Both attest-pq variants carry receipt version `c8s/attest-pq/v1` and the domain
tag `c8s-verify/v1`. They differ only in the session fields. For
`c8s/attest-pq/v1`, the fields are `session_pubkey.x25519` and
`session_pubkey.mlkem768`. For `+xwing`, they are `xwing_ek`, `xwing_ct`, and
`session_id`, all unpadded base64url. The response does not say which variant
it uses (production omits `c8s.attestationProtocol`). The registry therefore
pins it per release as `workload_attestation_protocol`.

The workload `front_door_mode` is the workload sidecar's own credential mode
(`webpki` on every workload on 2026-09-24). It is only a transcript input.
Workloads do not terminate public TLS, so the TEE-held-mode policy applies only
to the front door.

How each construction was confirmed:

- **`c8s/attest-pq/v1+xwing`** is specified in the MIT-licensed
  `confidential-dot-ai/c8s-verify-js` `PROTOCOL.md` ("Report-data and
  mesh-identity binding") and `src/identity.ts` `identityTranscriptHash`, at
  commit `d589419a5d61b63b6c5c086e2e4a5a073402a445`.
- **`c8s/attest-pq/v1`** (x25519 and ML-KEM-768) combines two MIT sources. The
  same repository's `src/identity.ts` at `3b48074^` (`54074f0`, before the
  X-Wing change) gives the field order: domain tag, CA hash, leaf hash, x25519,
  ML-KEM-768, nonce. Commit `d589419` shows where `LP(front_door_mode)` goes,
  right after the domain tag. No MIT source has both together; the combination
  was confirmed empirically.
- **Empirical check.** On 2026-09-24, live production and candidate
  `/attestation` responses were checked with the constructions above, 12
  workload receipts in total (6 per deployment). For every receipt, the
  recomputed digest equaled `report_data[0:48]`, the padding was zero, and the
  ECDSA identity proof verified under the chain-verified mesh leaf. The same
  constructions without `LP(front_door_mode)` did not match any receipt. The
  captured fixtures replay this check in
  `tests/provider_verifier/c8s_soundness.py`.
- `c8s/attest-lb/v1` is unchanged from the front-door adapter. MIT-licensed
  `confidential-dot-ai/TEErminator` `internal/verifier/endpoint.go` specifies
  it.

No AGPL `confidential-dot-ai/c8s` code was used.

## Allowlist canonicalization

The allowlist digest is SHA-256 over the canonical bytes of the
`c8s.allowlist/v1` document. The canonical form is Go's `encoding/json`
`json.Marshal` of c8s's allowlist struct:

- compact output, with no whitespace and no trailing newline;
- struct fields in declaration order;
- map keys (`digests`, `workloads`, `env.values`) sorted;
- Go string escaping: `<`, `>`, `&`, U+2028, and U+2029 as `\u` escapes, and
  `\b`, `\f`, `\n`, `\r`, and `\t` in short form (Go 1.22 and later; c8s
  builds with Go 1.27);
- non-ASCII characters as raw UTF-8.

`/attestation` embeds the document re-serialized with sorted keys, so its bytes
are not canonical. The adapter re-encodes the parsed document with this field
order:

| Object | Field order |
| --- | --- |
| allowlist | `schema`, `digests`, `workloads` |
| workload | `label`, `initContainers`, `containers`, `secrets` |
| container | `digest`, `image`, `command`, `args`, `mounts`, `env` |
| `command`, `args` | `policy`, `argv` |
| `mounts` | `policy`, `destinations`, `rules` |
| mount rule | `destination`, `kind`, `source`, `readOnly` |
| `env` | `policy`, `names`, `values` |
| `secrets` | `policy`, `read`, `write` |

It keeps a field only if the document has it. An unknown field, a number, a lone
surrogate, or a schema other than `c8s.allowlist/v1` fails closed. Different
c8s versions have used `mounts.destinations` or `mounts.rules`, and `env.names`
or `env.values`. The table covers both, so the check keeps working after the
provider pins env and mounts. The digest check is what guarantees correctness:
a wrong field order or escape can only fail verification. It cannot accept a
document the digest does not name.

How this was confirmed on 2026-09-24:

- MIT-licensed `confidential-dot-ai/TEErminator` (`internal/verifier/allowlist.go`,
  commit `8d302348`) hashes the bytes served at `GET /allowlist` verbatim.
  `c8s-verify-js` `PROTOCOL.md` also says these are "canonical allowlist bytes",
  hashed as received and never re-serialized.
- On both endpoints, the SHA-256 of the `GET /allowlist` bytes equals
  `c8s.activeAllowlist.sha256` and every workload stamp's digest. Those bytes
  parse to exactly the embedded `document`. They are compact, with no trailing
  newline, and their key order is the table above.
- Re-encoding the embedded `document` with the table above reproduces the served
  bytes byte for byte, for both production (17,931 bytes) and candidate
  (14,463 bytes). Sorted-key, indented, or newline-terminated serializations
  do not.
- The field declaration order, including the `destinations`/`rules` and
  `names`/`values` fields that no live document uses yet, was read from the
  AGPL `confidential-dot-ai/c8s` `pkg/allowlist/allowlist.go` (HEAD
  `ec67bd84`, and before commit `75af991` for `digests`). Its package comment
  states that the canonical form is `json.Marshal` of the normalized struct. No
  c8s code was copied. The adapter's encoder is written from the table above.

The verifier does not fetch `/allowlist`. It canonicalizes the embedded document
instead, so each verification is still a single `/attestation` request.

## Admission env and mounts

Audit finding, verified on 2026-09-24: both deployments admit `gateway`,
`sglang-router`, and every `inference-worker-*` with `env: {policy: any}` and
`mounts: {policy: any}` on every container, including the c8s sidecars. Only the
image digest and argv are exact (`confidential-inference`
`scripts/regenerate-c8s-allowlist.py:193` at `a54319a`). `gateway-state-mounter`
has the same policies, and it runs a ConfigMap-supplied script as privileged
root.

Env and mounts are not measured anywhere: not in a quote, not in RTMR3, and not
in the mesh leaf. So any Kubernetes API writer can change them without changing
anything the adapter verifies. For example:

- `GATEWAY_ALLOW_DIRECT_INFERENCE_URL` plus `GATEWAY_INFERENCE_URL` redirects
  prompts out of the attested path.
- `LD_PRELOAD` or `PYTHONPATH`, together with a ConfigMap mount, injects code
  into an attested image.
- A changed ConfigMap script runs as privileged root in `gateway-state-mounter`.

The adapter derives the policy from the digest-checked allowlist document
(algorithm step 15) and reports it:

- `admission_env_mounts_pinned`: `true` only if every checked entry is pinned.
- `admission_env_mounts`: keyed by required target, plus
  `gateway-state-mounter`. Each value holds the `allowlist_entry` name,
  `pinned`, and each container's `kind`, `digest`, `image`, `env` policy, and
  `mounts` policy.
- `require_pinned_env_mounts`: the release's policy.

`serving_software_known_good` is **refuted** while
`admission_env_mounts_pinned` is `false`. The reason names the unconstrained
entries. When it is `true`, the claim stays unknown, because the adapter still
does not review the images themselves.

**Interim policy.** As with RTMR3, the provider has not fixed this in any
release. Both current releases set `require_pinned_env_mounts: false`, so they
verify and report the gap instead of failing.

**Launch requires** a reviewed release with `require_pinned_env_mounts: true`.
Its allowlist must pin env and mounts (`exact` or `deny`) for every container of
the plaintext-path workloads and of `gateway-state-mounter`. With that setting,
today's allowlists fail closed, as the soundness test shows. Pinning is
necessary but not sufficient. The reviewer must also check that the exact env
values and mount rules do not re-open the gap, for example by allowing a
ConfigMap mount whose contents are not measured.

## Release registry

`provider_refs/c8s.json` is keyed by release ID. Each entry records:

- the release bundle digest and where it was published
- whether the bundle is signed
- the c8s policy mode
- MRTD, RTMR1, and RTMR2
- the accepted RTMR3 values, with their source
- the accepted allowlist digests
- the accepted mesh CA digests, when the bundle publishes one
- `workload_attestation_protocol`: the reviewed attest-pq variant
- `required_workloads`: `{target, workload, identity}` for `gateway`,
  `sglang-router`, and every `inference-worker-*`, copied from the bundle's
  `c8s.attestationTargets`
- `additional_admission_entries`: allowlist entries outside `receipts[]` whose
  env and mounts policy is also checked (`gateway-state-mounter`)
- `require_pinned_env_mounts`: whether an unpinned env or mounts policy fails
  verification (`false` for both current releases; launch requires `true`)

The loader rejects an entry that has no RTMR3 set, no allowlist, an unknown
workload protocol, a missing or non-boolean `require_pinned_env_mounts`, or
duplicate targets. It also rejects an entry whose
`required_workloads` omits `gateway`, `sglang-router`, or all inference
workers.

MRTD, RTMR1, RTMR2, the allowlist digest, the attestation targets, and (for
`v0.13.28-rc.2`) the mesh CA digest come from
`confidential-dot-ai/confidential-inference`
`releases/production/release-bundle.json` (`c8s.measurements`,
`allowlistDigest`, `c8s.attestationTargets`, and `c8s.meshCa`). Both bundles'
SHA-256 digests match the `release.bundleSha256` that the live endpoints
report. The bundles do not publish RTMR3 or the attest-pq variant. Each accepted
RTMR3 therefore records `"source": "observed-live"` and the observation date,
and the variant comes from the empirical check above.

| Release | Endpoint on 2026-09-24 | Bundle | Policy mode | Workload protocol |
| --- | --- | --- | --- | --- |
| `v0.13.27-static-attestation-20260910` | production | unsigned, from commit `475c35d` | static | `c8s/attest-pq/v1` |
| `v0.13.28-rc.2` | candidate | signed pre-release (Sigstore bundle asset recorded, not verified by this adapter) | operator | `c8s/attest-pq/v1+xwing` |

The adapter never fetches bundles or measurements at verification time. To
accept a new release:

1. Review the bundle.
2. Confirm that its digest matches the live `release.bundleSha256`.
3. Copy the plaintext-path targets from `c8s.attestationTargets`.
4. Confirm that the workload transcript variant matches live receipts.
5. Record RTMR3 from verified live quotes. Every required node must report an
   accepted value.
6. Review the allowlist's env and mounts policies. Set
   `require_pinned_env_mounts` to `true` only when every checked entry is
   pinned and the pinned values are safe.
7. Add the entry in a reviewed gateway change.

A release that is not in the registry fails closed.

The registry ships with the gateway, so accepting a release takes a gateway
redeploy. The provider published two releases on 2026-09-23 alone. Without
notice before rollout, a provider release makes this upstream fail
verification until the registry catches up. So does a change to the worker
set, such as scaling to `inference-worker-2`. Deployments that use this
upstream as a fallback should not rely on it until the provider commits to
publishing releases before rolling them out (audit criterion 7).

## RTMR3 and the operator key

A non-zero RTMR3 means the c8s operator key was armed at boot. The provider's
threat model states that this key can obtain cluster-admin, which can exec into
a pod inside the TEE and read workload memory. The product owner accepted
starting implementation while this remains open. The adapter therefore never
skips RTMR3. It accepts only the RTMR3 values listed for that release, on the
front-door node and on every workload node. Every value accepted today has the
key armed (`"operator_key_armed": true`).

The provider plans to remove the operator key and pin RTMR3 to zero. Zero is
not accepted today. A zero RTMR3 on a current release fails like any other
unlisted value. When a reviewed release ships with the key removed, add that
release with an RTMR3 of zero and `"operator_key_armed": false`. The
`os_known_good` claim then changes from refuted to asserted. It stays refuted
while any verified node's accepted RTMR3 has the key armed.

The soundness test covers this transition with a hypothetical zero-only entry
(no real zero entry exists). Today's non-zero quotes fail against it. Quotes
that report zero on every node verify with `operator_key_armed: false`. One
armed node keeps the whole upstream armed. The Rust claim test
`c8s_refutes_the_os_while_the_operator_key_is_armed` maps
`operator_key_armed: false` to an asserted `os_known_good`.

## Verified claims

| Claim | Result |
| --- | --- |
| TEE attested | Asserted (hardware-proven): DCAP-verified TDX quotes for the front door (report_data binds the nonce and the serving TLS leaf) and for each plaintext-path workload (report_data binds the nonce and the workload's mesh identity). |
| Platform TCB current | Asserted only when every quote is `UpToDate`; any other status fails verification. |
| OS known good | Refuted while any accepted RTMR3 arms the operator key. Asserted once a reviewed release accepts only RTMR3 values with the key removed. |
| Serving software known good | Refuted while the allowlist admits a plaintext-path workload or `gateway-state-mounter` with env or mounts policy `any` (true of both releases on 2026-09-24). Otherwise unknown: the images are not reviewed, and the mesh CA's own TEE evidence is not verified. |
| GPU attested | Unknown. The bridge emits `gpu_verified: false` and never fails on GPU evidence. |
| Model weights provenance | Unknown. The bundle pins a model revision and dm-verity root, and the workers' stamped identities name the reviewed worker workloads. The adapter does not verify the weights. |

`provider_claims` also records `tcb_status`, `release_id`, `bundle_sha256`,
`bundle_signed`, `policy_mode`, `mesh_ca_sha256`, `allowlist_sha256`,
`front_door_mode`, `tls_mode`, the front door's measured registers,
`rtmr3_source`, `operator_key_armed`, `workload_attestation_protocol`,
`admission_env_mounts_pinned`, `admission_env_mounts`,
`require_pinned_env_mounts`, the
matched `registry_entry`, and the provider's own `scope` and
`operationalStatus` (`launch-or-admission-only` and `not-verified` on
2026-09-24).

`verified_workloads` maps each required target to what it matched:

```json
"inference-worker-0": {
  "workload": "inference-worker-0",
  "identity": "inference-worker-0",
  "release_id": "v0.13.28-rc.2",
  "attestation_protocol": "c8s/attest-pq/v1+xwing",
  "front_door_mode": "webpki",
  "allowlist_sha256": "sha256:eb60…0dcc",
  "tcb_status": "UpToDate",
  "mrtd": "9309…8ba1",
  "rtmr1": "70f5…de6c",
  "rtmr2": "f57a…5b59",
  "rtmr3": "3e70…091f",
  "rtmr3_source": "observed-live",
  "operator_key_armed": true,
  "rtmr0": "d695…"
}
```

## What a tamper rejects

`tests/provider_verifier/c8s_soundness.py` replays live production and
candidate captures. Every quote is verified by the real `dcap_qvl` against the
collateral captured with it. Each tamper must fail closed.

Front door and release:

| Tamper | Rejected by |
| --- | --- |
| Serving leaf swapped for another endpoint's leaf | receipt `serving_leaf_sha256` mismatch |
| Serving leaf swapped and receipt hash rewritten | identity proof signature over the transcript |
| Different client nonce | top-level nonce mismatch |
| Receipt nonce changed | receipt nonce mismatch |
| Old receipt replayed with rewritten nonce fields | identity proof signature over the transcript |
| Verified report_data changed | transcript comparison |
| Non-zero report_data padding | padding check |
| Quote bytes changed | DCAP signature verification |
| Debug TD | `td_attributes` TUD check |
| TCB `OutOfDate` or `SWHardeningNeeded` | TCB status must be `UpToDate` |
| MRTD, RTMR1, or RTMR2 not pinned | release measurement pins |
| RTMR3 zero, from another release, or removed from the accepted set | accepted RTMR3 set |
| Unknown release, or bundle digest mismatch | release registry |
| Allowlist digest not accepted | accepted allowlist set |
| Allowlist document edited (env and mounts pinned) without a matching digest | canonical digest of the document |
| Allowlist document missing, out of schema, or with an unknown field | canonicalizer fails closed |
| Allowlist document and digest both replaced, even with the registry accepting the new digest | workload stamps name the original digest |
| `additional_admission_entries` names an entry the allowlist lacks | admission entry lookup |
| Release with `require_pinned_env_mounts: true` on today's allowlists | unpinned env and mounts |
| `tls.mode` or signed `front_door_mode` set to `webpki` | TEE-held mode check |
| Missing front door | front-door receipt required |
| Identity proof signature changed | ECDSA verification under the mesh leaf key |
| `c8s.meshCaSha256` changed | mesh CA digest check |
| Mesh CA replaced by another CA | leaf-to-CA chain check |
| Mesh CA not in a release's accepted set | accepted mesh CA set |
| Mesh leaf expired | certificate validity check |
| Non-HTTPS origin or origin with a path | origin validation |
| Registry entry without RTMR3, workload protocol, or a required router or worker target | registry loader |

Workloads:

| Tamper | Rejected by |
| --- | --- |
| `receipts[]` missing, or the `inference-worker-1` receipt removed | required target set |
| `gateway` receipt duplicated | one receipt per required target |
| Extra `inference-worker-2` receipt not in the release | unreviewed inference worker |
| `c8s.attestationProtocol` differs from the reviewed variant | workload protocol pin |
| Worker `identity` or `workload` label changed | target binding |
| Two genuine worker receipts swapped between targets | matched-workload stamp in the CA-signed leaf |
| Worker receipt nonce changed | workload receipt nonce mismatch |
| Worker receipt version changed | attest-pq version check |
| Worker session key, X-Wing ciphertext, or `front_door_mode` changed | workload identity proof over the attest-pq transcript |
| Worker `session_id` of the wrong size | session field size check |
| Worker identity proof signature changed | ECDSA verification under the workload mesh leaf |
| Worker verified report_data changed (`+xwing` and legacy) | attest-pq transcript comparison |
| Worker quote bytes changed | DCAP signature verification |
| Worker chain from another cluster, or worker leaf re-parented to another CA | same mesh CA as the front door |
| Worker debug TD | `td_attributes` TUD check |
| Worker or router TCB `OutOfDate` or `SWHardeningNeeded` | TCB status must be `UpToDate` |
| Worker MRTD or RTMR1, gateway RTMR2, or candidate worker RTMR1 not pinned | release measurement pins |
| Worker RTMR3 zero or from another release | accepted RTMR3 set |
| Candidate release pinned to the other attest-pq variant | session field decoding |
| Malformed matched-workload stamp (absent, trailing bytes, wrong version, long-form length, short digest, bad grammar) | strict stamp parser |

## Performance

The bridge verifies 5 quotes per call: the front door and the 4 required
workloads. Collateral fetches and DCAP checks run concurrently, at most 4 at a
time. Collateral is cached per FMSPC and PCK CA for the duration of one
verification, so production (one FMSPC) needs one fetch and candidate (two
FMSPCs) needs two.

On 2026-09-24, one bridge process run through `uv` took 3.6 s end to end
against production and 4.4 s against candidate. An instrumented in-process
production run took 2.6 s:

| Step | Time |
| --- | --- |
| `GET /attestation` (about 1.3 MB) | 1.56 s |
| One shared collateral fetch (PCCS) | 0.96 s |
| 5 DCAP verifications | about 2 ms each |

The response fetch and one collateral round trip dominate. Without the per-FMSPC
cache, each quote would fetch the same collateral again.

## Trust boundaries and limitations

- The trust boundary is the front door plus the `gateway`, `sglang-router`, and
  `inference-worker-*` workloads, each on a node that matches the reviewed
  release. `metrics-collector` and `kube-state-metrics` are not verified. The
  provider declares `scope: "launch-or-admission-only"` and
  `operationalStatus: "not-verified"`.
- Workload receipts prove that measured nodes, holding leaves from the front
  door's mesh CA, run the reviewed workload identities right now. They do not
  bind the forwarded request to a specific worker. Routing inside the cluster
  relies on the mesh: peers authenticate with leaves from that CA, and the CA
  stamps only allowlist-admitted workloads. The adapter does not verify the mesh
  CA's embedded RA-TLS evidence. It checks the allowlist document against its
  digest but reviews only its env and mounts policies.
- Env and mounts are unconstrained on every accepted release (see
  [Admission env and mounts](#admission-env-and-mounts)).
- A worker that the router can reach but that has no receipt in `receipts[]`
  is not detected. An unlisted `inference-worker-*` receipt fails, but the
  adapter cannot see a worker the response omits.
- In `+xwing` receipts, the X-Wing key exchange belongs to a session between the
  provider's attestation aggregator and the workload, not to the gateway. The
  adapter uses `xwing_ek`, `xwing_ct`, and `session_id` only as transcript
  inputs. It does not use that session for traffic.
- The mesh CA changes when CDS restarts. The CA of a release with a pinned mesh
  CA must be updated in the registry, or verification fails.
- The operator key remains armed on every accepted release (see above).
- The production bundle is unsigned. The candidate bundle is a signed
  pre-release. The adapter does not verify Sigstore signatures; the registry
  review does.
- The verifier requires a WebPKI-valid serving certificate for its own
  `/attestation` fetch. A `cds`-mode front door with a non-public certificate is
  accepted by policy but fails at that TLS handshake.
- GPU evidence is not verified. Production returned `raw-receipt-evidence`
  (raw NVIDIA evidence copied from worker receipts) and candidate returned
  `not-exposed-by-c8s`. Do not claim a GPU TEE for this provider.
- The provider emits no per-response signature. Receipts rely on the pinned
  channel.

## Test the adapter

```bash
uv run python tests/provider_verifier/c8s_soundness.py
cargo test c8s
```

`cargo test --test c8s_bridge` runs the soundness script through `uv`. The
fixtures in `tests/fixtures/c8s/` are live `/attestation` responses captured on
2026-09-24. Each includes the serving leaf from the same connection and the DCAP
collateral for every FMSPC in the response, valid at capture time. Fields
outside the transcripts, quotes, mesh chains, identity proofs, and the active
allowlist were removed to keep the files small (`c8s.discovery`, GPU evidence
items, event logs, `receipts[].admittedLaunch`, and worker `nvidia_gpu`). Each
fixture lists the removed fields in `trimmed`.

For a live check against the unauthenticated attestation endpoint:

```bash
printf '%s' '{"provider":"c8s","url_origin":"https://api.confidential.ai","model_id":"m"}' \
  | uv run python scripts/private_ai_provider_verifier.py
```

A successful result has `result: "verified"`, `attested_scope: "router"`, one
`tls_spki_sha256` binding, the matched release in
`provider_claims.registry_entry`, and one entry per required target in
`provider_claims.verified_workloads`.
