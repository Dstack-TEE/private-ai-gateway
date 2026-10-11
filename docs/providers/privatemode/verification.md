# Privatemode co-deployed proxy verification

- **TEE:** AMD SEV-SNP or Intel TDX with NVIDIA Confidential Computing
- **Session binding:** `proxy_image_sha256`, defined by the
  [proxy-image binding extension](../../../spec/proxy-image-binding.md)
- **Verifier:** official `privatemode-proxy` co-deployed in the gateway's
  measured dstack Compose
- **Transport:** the gateway's attested client channel, private Compose HTTP
  to the proxy, then Privatemode full-body E2EE to model workers
- **Manifest mode:** dynamic
- **Audit:** see [review.md](review.md)

## Trust boundary

The official proxy verifies the Contrast Coordinator, obtains the Mesh CA,
exchanges an inference secret, and encrypts inference bodies with that secret.
The gateway delegates the complete protocol to the proxy instead of
reimplementing individual quote checks.

dstack launches the gateway and proxy as separate services in one measured
Compose workload. The measurement binds the proxy image, command, credential
digest, private network topology, and shared manifest-history volume. The proxy
port is not published. Mutable upstream configuration cannot change the proxy
origin, supply another credential, or select a proxy path.

The proxy uses dynamic manifest mode. It fetches the current manifest when it
needs a Mesh CA and verifies the Coordinator against those exact bytes before
using the CA. It calls `LatestSecret` before each encrypted inference attempt.
An expired secret that cannot be refreshed fails the request.

Clients reach Privatemode routes as they reach any other provider. The
gateway's client-facing channel, including any E2EE v2 support, is the same for
every route.

## What the manifest observation proves

Privatemode v1.48 writes every fetched manifest to
`<workspace>/manifests/log.txt`, the manifest history its documentation
describes for audit. The documentation does not specify the line format; the
gateway parses the v1.48 format (`<RFC 3339 time> <path>/<N>.json`, from
`internal/manifestlog` in the tagged source) and resolves `<N>.json` next to
the log. Recheck that format whenever the pinned proxy image changes. The
gateway reads the latest version from the shared read-only manifest-history
volume and reports:

- `observed_manifest_sha256`
- `manifest_observed_at`
- `manifest_observation: "latest-proxy-fetch-log"`
- `manifest_bound_to_active_secret: false`

This is useful update visibility, but it is not a channel binding. The proxy
writes the history entry before Coordinator verification and before secret
exchange completes. The v1.48 API does not expose the manifest associated with
the secret used for a request. A receipt therefore must not use the observation
to assert manifest-specific GPU, OS, serving-software, or model-weight claims.

The observation may also be older than the request by the route's
`verifier_cache_seconds` lease. Its timestamp states when the proxy fetched the
manifest, not when the gateway emitted the receipt.

## Verification and forwarding

At startup, the gateway validates the mounted API credential against its
measured SHA-256 digest. It retains no credential bytes in deployment state.
The measured proxy image is pinned by OCI digest.

For route verification, the gateway:

1. Sends `GET /readyz`, the proxy's readiness endpoint, to the pinned internal
   proxy origin, and rejects redirects, ambient HTTP proxies, and non-success
   status.
2. Reads and validates the latest manifest-history entry, falling back to the
   previous one while the proxy is still writing the newest file. The
   observation is the session's evidence bundle, which every sealed session
   must carry.
3. Emits the measured proxy binding and the explicitly unbound manifest
   observation.

With `--apiKey` set, the proxy starts listening only after its initial Contrast
verification and secret exchange succeed, so a readiness answer corroborates
startup and liveness. It does not use or identify the current inference secret.

For inference, the gateway permits only these v1.48 handlers, all of which the
proxy encrypts. The proxy also encrypts transcription, Anthropic Messages, and
unstructured handlers, which the gateway does not expose; it forwards
`/v1/models` unencrypted.

- `/v1/chat/completions`
- `/v1/completions`
- `/v1/embeddings`

The gateway sends no internal Bearer token. The proxy applies its measured
startup credential outbound. The forwarding client rejects redirects and
ignores HTTP proxy environment variables.

Plain HTTP is intentional on the internal hop. Both services and the private
network are inside the same attested dstack workload. Privatemode E2EE protects
the request after it leaves that workload.

## Session binding and claims

The receipt's `upstream.verified` event cites an immutable attested session.
That session contains the enforceable channel binding that the
[proxy-image binding extension](../../../spec/proxy-image-binding.md) defines:

```json
{
  "type": "proxy_image_sha256",
  "provider": "privatemode",
  "proxy_image_digest": "sha256:...",
  "credential_sha256": "..."
}
```

The session's `endpoint` is the measured internal origin. Its verifier ID is
`privatemode-proxy/co-deployed-contrast/v1`.

The session asserts `tee_attested` as `VerifierDerived`: the measured proxy must
establish a Contrast-attested E2EE secret before it starts serving. These claims
remain `Unknown` until the proxy exposes a request-bound active manifest:

- `gpu_attested`
- `tcb_up_to_date`
- `os_known_good`
- `serving_software_known_good`
- `model_weights_provenance`

The full observed manifest is retained as session evidence with
`bound_to_active_secret: false`. The observation metadata is also in
`claims.extra`; the receipt contains only the session id and verification
outcome.

## Failure behavior

The route fails closed when:

- static proxy policy is absent or its origin does not match the route;
- the credential is missing, malformed, or has the wrong digest;
- the proxy image digest is malformed;
- mutable configuration supplies a Bearer token or path;
- the readiness probe fails;
- the manifest log is malformed or its latest file is missing or unreadable;
- forwarding targets a handler outside the encrypted allowlist; or
- the verified proxy-image binding differs from the active deployment.

There is no fallback to the public Privatemode API, another proxy, an HTTP
redirect, or an ambient HTTP proxy.

## Configuration and updates

Use the [deployment runbook](../../../deploy/privatemode.md#verify-the-deployment)
and [configuration reference](../../configuration-reference.md#privatemode-proxy).

The official proxy refreshes the manifest when secret refresh needs a new Mesh
CA. A proxy-image or credential change requires a measured redeployment. A
manifest update does not require a redeployment, but relying parties should
review the new observed digest before assigning manifest-specific transitive
claims outside the gateway.

## Tests and reproduction

Run the hermetic adapter tests: the credential and binding refusals, redirect
refusal, readiness deadline, verification cache, manifest observation, receipt
binding, and capacity retry:

```sh
cargo test --test provider_e2e privatemode
```

For a live run, start the pinned proxy yourself and add a `privatemode` entry
to the [live end-to-end suite](../../live-e2e-test-suite.md#provider-matrix-format).
To test a Phala deployment, follow the
[deployment runbook](../../../deploy/privatemode.md#verify-the-deployment).

## Sources

- [Privatemode attestation overview](https://docs.privatemode.ai/architecture/attestation/overview/)
- [Privatemode proxy configuration](https://docs.privatemode.ai/api/proxy-configuration/)
- [Privatemode TCB source](https://github.com/edgelesssys/privatemode-public)
- [Contrast source](https://github.com/edgelesssys/contrast)
