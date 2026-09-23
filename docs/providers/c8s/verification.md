# Confidential AI (c8s) Verification

This reference is for Private AI Gateway operators who configure Confidential
AI (`https://api.confidential.ai`, operated by Inexorable, Inc.). Confidential
AI runs on c8s (Confidential Kubernetes). Each node is an Intel TDX CVM. The
`c8s-tls-lb` front door terminates public TLS inside that CVM. The gateway
verifies the front door and the node measurements, and then pins the serving
TLS key for forwarded traffic.

The admission review is `docs/providers/confidential-ai/review.md` (on the
review branch). That review lists a hard reject that is still open: the c8s
operator key can reach cluster-admin. This adapter does not resolve it. It
records the key state honestly (see [RTMR3 and the operator key](#rtmr3-and-the-operator-key)).

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
6. Requires `frontDoor.source` to be `c8s-tls-lb` and requires a receipt with
   version `c8s/attest-lb/v1` and platform `tdx`. The receipt `nonce` must
   equal the client nonce. The signed `front_door_mode` must be a TEE-held mode
   equal to `tls.mode`. `serving_leaf_sha256` must name the leaf observed in
   step 2.
7. Parses `cds_cert_pem` as exactly one mesh leaf and one mesh CA. The CA must
   be a self-signed CA certificate, and the leaf must be issued by it. Both
   must be inside their validity period. `c8s.meshCaSha256` must equal the
   SHA-256 of the mesh CA DER. If the registry lists accepted mesh CAs for the
   release, the CA must be one of them.
8. Recomputes the attest-lb transcript digest from the client nonce and the
   observed serving leaf. `LP(x)` is a 4-byte big-endian length followed by
   `x`:

   ```text
   SHA-384( LP("c8s/attest-lb/v1") || LP(front_door_mode) || LP(nonce_raw_32) ||
            LP(SHA-256(serving_leaf_DER)) || LP(SHA-256(mesh_leaf_DER)) ||
            LP(SHA-256(mesh_CA_DER)) )
   ```

9. Verifies `identity_proof`. The algorithm must be `ecdsa-sha384`.
   `leaf_sha256` and `mesh_ca_sha256` must name the returned certificates. The
   signature must verify under the mesh leaf key, using ECDSA with SHA-384 over
   the 48-byte transcript digest.
10. Requires `c8s.activeAllowlist.sha256` to be in the release's accepted set.
11. Decodes the front-door quote as hex or base64 (standard or URL-safe). It
    must be a TDX v4 quote. The bridge verifies the quote with `dcap_qvl`
    against Intel PCS collateral and requires TCB status `UpToDate`.
12. Reads the verified TD report. It rejects a debug TD (any TUD bit in
    `td_attributes` byte 0). It requires `report_data[0:48]` to equal the
    transcript digest and `report_data[48:64]` to be zero.
13. Requires MRTD, RTMR1, and RTMR2 to equal the release pins, and RTMR3 to be
    in the release's accepted RTMR3 set.
14. Emits a `tls_spki_sha256` binding for the observed serving leaf. The
    TEE holds the key, so the SPKI pin is equivalent to the leaf binding. The
    Rust forwarding path enforces the pin on every model request.

RTMR0 is not pinned. It carries the per-host virtual firmware configuration
and differs between production and candidate nodes that run the same image.

The scope is `router`. One origin and one SPKI serve every configured model.

## Release registry

`provider_refs/c8s.json` is keyed by release ID. Each entry records:

- the release bundle digest and where it was published
- whether the bundle is signed
- the c8s policy mode
- MRTD, RTMR1, and RTMR2
- the accepted RTMR3 values, with their source
- the accepted allowlist digests
- the accepted mesh CA digests, when the bundle publishes one

MRTD, RTMR1, RTMR2, the allowlist digest, and (for `v0.13.28-rc.2`) the mesh CA
digest come from `confidential-dot-ai/confidential-inference`
`releases/production/release-bundle.json` (`c8s.measurements`,
`allowlistDigest`, and `c8s.meshCa`). Both bundles' SHA-256 digests match the
`release.bundleSha256` that the live endpoints report. The bundles do not
publish RTMR3, so each accepted RTMR3 records `"source": "observed-live"` and
the observation date.

| Release | Endpoint on 2026-09-23 | Bundle | Policy mode |
| --- | --- | --- | --- |
| `v0.13.27-static-attestation-20260910` | production | unsigned, from commit `475c35d` | static |
| `v0.13.28-rc.2` | candidate | signed pre-release (Sigstore bundle asset recorded, not verified by this adapter) | operator |

The adapter never fetches bundles or measurements at verification time. To
accept a new release:

1. Review the bundle.
2. Confirm that its digest matches the live `release.bundleSha256`.
3. Record RTMR3 from a verified live quote.
4. Add the entry in a reviewed gateway change.

A release that is not in the registry fails closed.

The registry ships with the gateway, so accepting a release takes a gateway
redeploy. The provider published two releases on 2026-09-23 alone. Without
notice before rollout, a provider release makes this upstream fail
verification until the registry catches up. Deployments that use it as a
fallback should not rely on it until the provider commits to publishing
releases before rolling them out (audit criterion 7).

## RTMR3 and the operator key

A non-zero RTMR3 means the c8s operator key was armed at boot. The provider's
threat model states that this key can obtain cluster-admin, which can exec into
a pod inside the TEE and read workload memory. The product owner accepted
starting implementation while this remains open. The adapter therefore never
skips RTMR3. It accepts only the RTMR3 values listed for that release. Every
value accepted today has the key armed (`"operator_key_armed": true`).

The provider plans to remove the operator key and pin RTMR3 to zero. Zero is
not accepted today. A zero RTMR3 on a current release fails like any other
unlisted value. When a reviewed release ships with the key removed, add that
release with an RTMR3 of zero and `"operator_key_armed": false`. The
`os_known_good` claim then changes from refuted to asserted.

## Verified claims

| Claim | Result |
| --- | --- |
| TEE attested | Asserted (hardware-proven): DCAP-verified TDX quote whose report_data binds the nonce and the serving TLS leaf. |
| Platform TCB current | Asserted only for `UpToDate`; any other status fails verification. |
| OS known good | Refuted while the accepted RTMR3 arms the operator key. Asserted once a reviewed release accepts an RTMR3 with the key removed. |
| Serving software known good | Unknown. The allowlist digest must be in the reviewed set, but the adapter reads it from the response JSON; this adapter does not bind it to the quote. |
| GPU attested | Unknown. The bridge emits `gpu_verified: false` and never fails on GPU evidence. |
| Model weights provenance | Unknown. The bundle pins a model revision and dm-verity root, but the adapter does not verify the worker receipts. |

`provider_claims` also records `tcb_status`, `release_id`, `bundle_sha256`,
`bundle_signed`, `policy_mode`, `mesh_ca_sha256`, `allowlist_sha256`,
`front_door_mode`, `tls_mode`, the measured registers, `rtmr3_source`,
`operator_key_armed`, the matched `registry_entry`, and the provider's own
`scope` and `operationalStatus` (`launch-or-admission-only` and `not-verified`
on 2026-09-23).

## What a tamper rejects

`tests/provider_verifier/c8s_soundness.py` replays live production and
candidate captures. The quote is verified by the real `dcap_qvl` against the
collateral captured with it. Each tamper must fail closed:

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
| `tls.mode` or signed `front_door_mode` set to `webpki` | TEE-held mode check |
| Missing front door | front-door receipt required |
| Identity proof signature changed | ECDSA verification under the mesh leaf key |
| `c8s.meshCaSha256` changed | mesh CA digest check |
| Mesh CA replaced by another CA | leaf-to-CA chain check |
| Mesh CA not in a release's accepted set | accepted mesh CA set |
| Mesh leaf expired | certificate validity check |
| Non-HTTPS origin or origin with a path | origin validation |

## Trust boundaries and limitations

- The trust boundary is the c8s front door on a measured node. The adapter
  does not verify the per-workload `receipts[]`, the mesh CA's embedded RA-TLS
  evidence, or the allowlist document contents. The provider declares
  `scope: "launch-or-admission-only"` and `operationalStatus: "not-verified"`.
- Mesh peers are authenticated by the mesh CA chain, not by per-peer
  measurement. The mesh CA changes when CDS restarts. The CA of a release with
  a pinned mesh CA must be updated in the registry, or verification fails.
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
2026-09-23 with the serving leaf from the same connection and the DCAP
collateral valid at capture time. Fields outside the transcript, quote, and
identity proof (`c8s.discovery`, the allowlist document, `receipts[]`, GPU
evidence items, and the event log) were removed to keep the files small.

For a live check against the unauthenticated attestation endpoint:

```bash
printf '%s' '{"provider":"c8s","url_origin":"https://api.confidential.ai","model_id":"m"}' \
  | uv run python scripts/private_ai_provider_verifier.py
```

A successful result has `result: "verified"`, `attested_scope: "router"`, one
`tls_spki_sha256` binding, and the matched release in
`provider_claims.registry_entry`.
