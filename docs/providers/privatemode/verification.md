# Privatemode co-deployed proxy verification

- **TEE:** AMD SEV-SNP or Intel TDX with NVIDIA Confidential Computing
- **Session binding:** `proxy_image_sha256`
- **Verifier:** official `privatemode-proxy` co-deployed in the gateway's
  measured dstack Compose
- **Transport:** private Compose HTTP to the proxy, then Privatemode full-body
  E2EE to model workers
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

## What the manifest observation proves

Privatemode v1.48 writes every fetched manifest to
`<workspace>/manifests/log.txt`. The gateway reads the latest version from the
shared read-only manifest-history volume and reports:

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

1. Sends an unauthenticated `GET /v1/models` to the pinned internal proxy
   origin.
2. Rejects redirects, ambient HTTP proxies, non-success status, non-JSON
   responses, and bodies larger than 1 MiB.
3. Reads and validates the latest manifest-history entry.
4. Emits the measured proxy binding and the explicitly unbound manifest
   observation.

The model-list probe corroborates proxy startup and liveness. It does not use or
identify the current inference secret.

For inference, the gateway permits only the encrypted v1.48 handlers:

- `/v1/chat/completions`
- `/v1/completions`
- `/v1/embeddings`
- `/v1/messages`

The gateway sends no internal Bearer token. The proxy applies its measured
startup credential outbound. The forwarding client rejects redirects and
ignores HTTP proxy environment variables.

Plain HTTP is intentional on the internal hop. Both services and the private
network are inside the same attested dstack workload. Privatemode E2EE protects
the request after it leaves that workload.

## Session binding and claims

The verified event contains:

```json
{
  "type": "proxy_image_sha256",
  "provider": "privatemode",
  "proxy_image_digest": "sha256:...",
  "credential_sha256": "..."
}
```

The event's `url_origin` is the measured internal origin. Its verifier ID is
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
`bound_to_active_secret: false`.

## Failure behavior

The route fails closed when:

- static proxy policy is absent or its origin does not match the route;
- the credential is missing, malformed, or has the wrong digest;
- the proxy image digest is malformed;
- mutable configuration supplies a Bearer token or path;
- the model-list probe fails;
- the manifest log is malformed or its latest file is missing or unreadable;
- forwarding targets a handler outside the encrypted allowlist; or
- the verified proxy-image binding differs from the active deployment.

There is no fallback to the public Privatemode API, another proxy, an HTTP
redirect, or an ambient HTTP proxy.

## Configuration and updates

Use the [deployment runbook](../../../deploy/README.md#verify-a-privatemode-deployment)
and [configuration reference](../../configuration-reference.md#privatemode-proxy).

The official proxy refreshes the manifest when secret refresh needs a new Mesh
CA. A proxy-image or credential change requires a measured redeployment. A
manifest update does not require a redeployment, but relying parties should
review the new observed digest before assigning manifest-specific transitive
claims outside the gateway.

## Sources

- [Privatemode attestation overview](https://docs.privatemode.ai/architecture/attestation/overview/)
- [Privatemode proxy configuration](https://docs.privatemode.ai/api/proxy-configuration/)
- [Privatemode TCB source](https://github.com/edgelesssys/privatemode-public)
- [Contrast source](https://github.com/edgelesssys/contrast)
