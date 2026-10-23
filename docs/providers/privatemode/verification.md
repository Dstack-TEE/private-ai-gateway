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

## Why the proxy origin and credential are not in the route

For Tinfoil, NEAR AI, Chutes, Phala direct, and SecretAI, the route carries
`base_url` and `bearer_token` in mutable
[upstream configuration](../../configuration-reference.md#upstream-fields),
supplied through the admin API or `upstream_pull` and controlled outside the TEE.
The gateway verifies the provider's attestation and enforces its
[`tls_spki_sha256` or `e2ee_public_key_sha256` binding](../README.md#provider-matrix)
on the actual connection. An endpoint without that attested binding cannot
receive the prompt. The token provides access and billing; the binding protects
prompts even when the route's origin changes.

Privatemode delegates attestation and E2EE to the official proxy. The gateway
sends it plaintext over private HTTP and cannot distinguish the real proxy from
an arbitrary plaintext HTTP server by that connection alone. A route-supplied
origin would let whoever controls mutable configuration redirect plaintext
prompts. The origin, image digest, and credential digest therefore belong in
the measured static
[`privatemode_proxy` policy](../../configuration-reference.md#privatemode-proxy).
The route's `base_url` must match that origin; `bearer_token` and `path` are
rejected.

The proxy reads its credential once at startup through `--api-key @file` for
its attested secret exchange and outbound authentication, with
`--api-key-fallback=always`. The key is delivered to the proxy as a Compose
secret. Its measured digest identifies the Privatemode account credential used
in the session's [proxy-image binding](../../../spec/proxy-image-binding.md).
Rotating it requires a measured redeployment.

| Provider group | Who verifies the provider | Endpoint | Credential |
| --- | --- | --- | --- |
| Tinfoil, NEAR AI, Chutes, Phala direct, SecretAI | Gateway | Route `base_url`, with attested binding enforced | Route `bearer_token` for access and billing |
| Privatemode | Official co-deployed proxy | Measured static origin | Compose secret with measured digest |

A native gateway implementation of Privatemode's protocol would remove the
sidecar and allow route-supplied endpoints and keys like the other providers;
this is not planned.

## Optional active-secret evidence

`privatemode_proxy.active_secret_evidence` defaults to `false`, preserving the
manifest observation below. Enable it only with a pinned proxy implementing the
[active-secret extension](../../../spec/privatemode-active-secret-evidence.md);
the reference v1.59 image lacks this API.

For every 2xx response, the gateway fetches evidence for `Privatemode-Secret-ID`
from the measured origin and binds the receipt's session before releasing
provider bytes. It checks the version 1 shape, encodings, sizes, timestamps,
manifest digest, and matching header/URL/bundle/public-key IDs. Redirects and
ambient proxies are rejected; fetches are bounded by ten seconds and 8 MiB,
with at most eight cached IDs. See the extension for exact limits and refusal
semantics. Evidence failures intentionally refuse even unconstrained requests;
non-2xx responses retain normal error/retry/failover handling.

Sessions carry `manifest_bound_to_active_secret: true`, `active_manifest_sha256`,
`secret_id`, and the full bundle. Discovery lists no bound session before a valid
response, then returns the most recently observed secret under the readiness
lease; the next request may use another secret. Session pins are rechecked after
response binding. GPU, OS, TCB, software, and model claims remain unknown.

The gateway trusts the measured proxy's cryptographic checks. The bound claims
identify which manifest its Coordinator attested for that secret; trusting the
manifest requires comparison against Privatemode's published CDN/release
manifest or a reviewed allowlist. The extension's
[verification procedure](../../../spec/privatemode-active-secret-evidence.md#sessions-and-verification)
uses `verify-evidence --accept-manifest-sha256 <hex>`; without an allowlist, PASS
proves only internal consistency. That verifier fetches AMD/Intel collateral
online and is not air-gapped.

## What the manifest observation proves

Privatemode v1.59 writes every fetched manifest to
`<workspace>/manifests/log.txt`, the manifest history its documentation
describes for audit. The documentation does not specify the line format; the
gateway parses the v1.59 format (`<RFC 3339 time> <path>/<N>.json`, from
[`internal/manifestlog`][manifestlog-source] in the tagged source) and resolves
`<N>.json` next to the log. Recheck that format whenever the pinned proxy image
changes. The gateway reads the latest version from the shared read-only
manifest-history volume and reports:

- `observed_manifest_sha256`
- `manifest_observed_at`
- `manifest_observation: "latest-proxy-fetch-log"`
- `manifest_bound_to_active_secret: false`

This is useful update visibility, but it is not a channel binding. The proxy
writes the history entry before Coordinator verification and before secret
exchange completes. The v1.59 API does not expose the manifest associated with
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
2. With active-secret evidence disabled, reads and validates the latest
   manifest-history entry, falling back to the previous one while the proxy
   is still writing the newest file. The
   observation is the session's evidence bundle, which every sealed session
   must carry.
3. Emits the measured proxy binding and the explicitly unbound manifest
   observation.

With `--api-key` set, the proxy starts listening only after its initial Contrast
verification and secret exchange succeed, so a readiness answer corroborates
startup and liveness. It does not use or identify the current inference secret.
The refresh loop now retries failed background updates with bounded backoff;
`LatestSecret` still fails closed if an expired secret cannot be refreshed.

For inference, the gateway permits only these v1.59 handlers, all of which the
proxy encrypts. The proxy also encrypts transcription, Anthropic Messages, and
unstructured handlers, which the gateway does not expose; it forwards
`/v1/models` unencrypted.

- `/v1/chat/completions`
- `/v1/completions`
- `/v1/embeddings`

The gateway sends no internal Bearer token. The proxy applies its measured
startup credential outbound because the Compose explicitly sets
`--api-key-fallback=always`. Upstream v1.59 warns that the default will change
to `never` in a future release; the explicit flag preserves this behavior.
As of 2026-10-19 PDT, the Compose uses the canonical `--api-key`,
`--nvidia-ocsp-allow-unknown=false`, and
`--nvidia-ocsp-revoked-grace-period=0` flags. The tagged v1.59.0 [CLI][cli-source]
shares each deprecated alias's flag value and backing variable with its
canonical spelling, including the same `@file` credential loading and whitespace
trimming. Switching spellings preserves behavior and removes deprecation
warnings. The strict OCSP settings remain an admission condition: only good
status is accepted, with no unknown-status tolerance or revoked-certificate
grace period. On each encrypted request, the proxy authenticates the OCSP
policy with an HMAC using the inference secret; the worker verifies that MAC
and enforces the policy against its driver, GPU, and VBIOS statuses. See the
[proxy handler][handler-source] and [worker enforcement][worker-source].

The proxy supports `--listen-address`; its default remains `0.0.0.0`, appropriate
for the private Compose network. The forwarding client rejects redirects and
ignores HTTP proxy environment variables.

Plain HTTP is intentional on the internal hop. Both services and the private
network are inside the same attested dstack workload. Privatemode E2EE protects
the request after it leaves that workload.

## Prompt cache

The reference Compose sets `--disable-prompt-cache=false`. The default
`--prompt-cache-salt random` generates one salt per proxy startup, so requests
without a caller-supplied salt share one cache namespace across all callers of
that proxy replica. Restarts generate a new namespace; replicas do not share
their startup salts. This removes the proxy's default per-request cache
isolation. Tinfoil's namespace is broader: it derives from the shared upstream
credential, spans replicas, and survives restarts. See
[Prefix-cache isolation](../README.md#prefix-cache-isolation-observation).

Sharing a prefix cache creates a cross-tenant prefix-timing side channel: a
caller can infer whether another caller recently sent the same prompt prefix.
Callers that need isolation can supply their own `cache_salt` (at least 32
bytes), which takes precedence. The proxy derives shard affinity from the
effective salt and prompt content, helping repeated prefixes reach the same
workers. This does not guarantee a cache hit or a particular worker. The
gateway preserves caller salts and does not derive a tenant salt.
See the tagged v1.59.0 [CLI][cli-source], [handler][handler-source], and
[shard-key implementation][shard-source].

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
remain `Unknown` in both modes; manifest review policy is not implemented:

- `gpu_attested`
- `tcb_up_to_date`
- `os_known_good`
- `serving_software_known_good`
- `model_weights_provenance`

With active-secret evidence disabled, the full observed manifest is retained as
session evidence with `bound_to_active_secret: false`. The observation metadata
is also in `claims.extra`; the receipt contains only the session id and verification
outcome.

## Failure behavior

The route fails closed when:

- static proxy policy is absent or its origin does not match the route;
- the credential is missing, malformed, or has the wrong digest;
- the proxy image digest is malformed;
- mutable configuration supplies a Bearer token or path;
- the readiness probe fails;
- with active-secret evidence disabled, the manifest log is malformed or its
  latest file is missing or unreadable;
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
binding, and capacity retry. Active-secret tests add exact-ID binding, rotation,
pinned sessions, missing/unknown/malformed evidence, bounded cache/body reads,
and buffered/streaming refusals:

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

[manifestlog-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/privatemode-proxy/internal/manifestlog/manifestlog.go
[cli-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/privatemode-proxy/cmd/cmd.go
[handler-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/privatemode-proxy/internal/server/server.go
[shard-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/internal/oss/mutators/mutators.go
[worker-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.59.0/inference-proxy/internal/adapter/inference/inference.go
