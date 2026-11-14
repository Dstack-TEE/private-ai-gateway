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

## What the manifest observation proves

Privatemode v1.64 writes the attested manifest before appending an entry to
`<workspace>/manifests/log.txt` after a successful secret exchange. Its format
is `<RFC 3339 time> <path>/<N>.json <secret ID>`; unchanged manifests reuse
the same numbered file across secret rotations. See the tagged
[logger][manifestlog-source] and [secret-exchange setup][setup-source]. The
gateway also accepts the legacy timestamp/path format, which v1.62 wrote before
Coordinator verification and secret exchange. It resolves `<N>.json` next to
the log, ignores an incomplete final line, and tolerates a pending manifest
file from older proxies. The gateway reports:

- `observed_manifest_sha256`
- `manifest_observed_at`
- `manifest_observation: "latest-proxy-fetch-log"`
- `observed_secret_id` when present in the log (omitted for legacy entries)
- `manifest_bound_to_active_secret: false`

The observation evidence also includes `secret_id` when present and always
sets `bound_to_active_secret: false`. The ID identifies a successful secret
exchange associated with that manifest on v1.64. It does not identify the
secret used for this gateway request. The [proxy handler][handler-source]
selects `LatestSecret` for each attempt and sets `Privatemode-Secret-ID` only
on the outbound request. Its response mapper does not return that selected ID
to the gateway. Concurrent requests, secret rotation, and retries prevent
inferring the request's ID from the newest log entry. A receipt therefore must
not assert manifest-specific GPU, OS, serving-software, or model-weight claims
on this basis.

The observation may also be older than the request by the route's
`verifier_cache_seconds` lease. On v1.64 its timestamp states when the proxy
logged the successful exchange; on older proxies it records a manifest fetch.
It is not the time the gateway emitted the receipt.

To prove request binding, upstream would need to expose the secret ID selected
for the final attempt in a response header from the measured proxy. A separate
native transport implementation could instead observe secret selection
directly. The gateway would then need to match
that ID against manifest history and seal the request's result after observing
the response, rather than equating a cached pre-request observation with the
request's secret. The pinned v1.64 proxy does not provide the response header.

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

With `--api-key` set, the proxy starts listening only after its initial Contrast
verification and secret exchange succeed, so a readiness answer corroborates
startup and liveness. It does not use or identify the current inference secret.
The refresh loop now retries failed background updates with bounded backoff;
`LatestSecret` still fails closed if an expired secret cannot be refreshed.

For inference, the gateway permits only these v1.64 handlers, all of which the
proxy encrypts. The proxy also encrypts transcription, Anthropic Messages, and
unstructured handlers, which the gateway does not expose; it forwards
`/v1/models` unencrypted.

- `/v1/chat/completions`
- `/v1/completions`
- `/v1/embeddings`

The gateway sends no internal Bearer token. The proxy applies its measured
startup credential outbound because the Compose explicitly sets
`--api-key-fallback=always`. Upstream v1.62 warns that the default will change
to `never` in a future release; the explicit flag preserves this behavior.
As of 2026-10-19 PDT, the Compose uses the canonical `--api-key`,
`--nvidia-ocsp-allow-unknown=false`, and
`--nvidia-ocsp-revoked-grace-period=0` flags. The tagged v1.62.0 [CLI][cli-source]
shares each deprecated alias's flag value and backing variable with its
canonical spelling, including the same `@file` credential loading and whitespace
trimming. Switching spellings preserves behavior and removes deprecation
warnings. The strict OCSP settings remain an admission condition: only good
status is accepted, with no unknown-status tolerance or revoked-certificate
grace period. On each encrypted request, the proxy includes the OCSP policy in
authenticated metadata inside the encrypted request. The worker requires that
metadata from v1.61 and newer clients, verifies it during decryption, and
enforces the policy against its driver, GPU, and VBIOS statuses before
forwarding to the model.
See the [proxy handler][handler-source] and [worker enforcement][worker-source].

The proxy supports `--listen-address`; its default remains `0.0.0.0`, appropriate
for the private Compose network. The forwarding client rejects redirects and
ignores HTTP proxy environment variables.

Plain HTTP is intentional on the internal hop. Both services and the private
network are inside the same attested dstack workload. Privatemode E2EE protects
the request after it leaves that workload.

## Proxy timeouts

The v1.62.0 [CLI][cli-source] exposes the incoming HTTP server's timeouts as
flags. Their defaults match the values already hard-coded in v1.59.0:
`--timeout-read=2m`, `--timeout-read-header=10s`, `--timeout-idle=1m`, and
`--timeout-write=0`. The upstream response-header timeout remains one hour,
now configurable with `--timeout-upstream-response-header`.

The two-minute read timeout limits receiving the request body, not waiting for
inference headers or completing a response. Before forwarding, the proxy's
`limitRequestBody` middleware consumes the body through
[`persist.ReadBody`][persist-source]. In Go 1.27's [`net/http` server][go-server-source],
body EOF starts the background disconnect reader, which clears the socket's
read deadline. `IdleTimeout` applies after the handler finishes while waiting
for the next keep-alive request; it does not limit an active response. With no
write timeout, long buffered inference and streaming responses remain supported.
The Compose therefore keeps the existing command unchanged.

On 2026-11-05 PST, the pinned image passed a controlled local HTTPS-upstream
test at these defaults: a request with a fully consumed body received no
headers for 130 seconds and then completed with HTTP 200. A second response
sent SSE headers immediately, paused for 65 seconds between chunks twice, and
completed at 130 seconds with HTTP 200 and exact expected bytes. The test used
`GET /v1/models/{model}` through the same body-reading middleware as encrypted
inference, without credentials or provider traffic. Separate minimal live
`glm-5.3` buffered and streaming calls passed through the proxy with attestation,
secret exchange, and strict OCSP settings; see [review.md](review.md).

## Prompt cache

The reference Compose sets `--disable-prompt-cache=false`. In authenticated
middleware mode, the gateway supplies a per-user, provider-separated
`cache_salt` (64 lowercase hex characters, satisfying the proxy's minimum 32
bytes). A non-empty caller `cache_salt` string subdivides that user's partition;
the caller's raw salt and `user_cache_secret` are removed. This replaces the
shared namespace implied by the proxy's default startup-random salt. See
[Prefix-cache isolation](../README.md#prefix-cache-isolation-observation) for
identity trust, provider limitations, and rotation.

Without a generated or valid caller salt (including anonymous middleware
traffic), the proxy's default `--prompt-cache-salt random` generates one salt
per startup. Those requests share a namespace within that replica until it
restarts. Direct gateway mode retains its existing caller-salt behavior.

The proxy derives shard affinity from the effective salt and prompt content.
The stable per-user salt can therefore make a user's traffic linkable through
shard keys visible to Privatemode's gateway. Provider separation prevents the
same pseudonym being sent to other providers, but does not remove this
within-provider linkability. Partitioning also reduces cache reuse between
users and can change worker affinity, throughput, and the frequency of
provider rate-limit responses. It guarantees neither a cache hit nor a
particular worker. Obtain appropriate provider quota; do not add quota
workarounds. The provider must honor the salt for isolation to hold.

See the tagged v1.62.0 [CLI][cli-source], [handler][handler-source], and
[shard-key implementation][shard-source]. Hermetic gateway coverage:
`cargo test --test middleware_completion cache_salt` checks authenticated and
anonymous requests, streaming, failover, caller sub-partitions, and receipt
hashes. A disposable-CVM run on 2026-11-12 confirmed per-user isolation
with the pinned proxy; see
[Prefix-cache isolation](../README.md#prefix-cache-isolation-observation).
Production rollout remains unverified.

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

[manifestlog-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.64.0/privatemode-proxy/internal/manifestlog/manifestlog.go
[setup-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.64.0/privatemode-proxy/internal/setup/setup.go
[cli-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.64.0/privatemode-proxy/cmd/cmd.go
[handler-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.64.0/privatemode-proxy/internal/server/server.go
[shard-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.64.0/internal/oss/mutators/mutators.go
[worker-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.64.0/inference-proxy/internal/adapter/inference/inference.go

[persist-source]: https://github.com/edgelesssys/privatemode-public/blob/v1.64.0/internal/oss/persist/persist.go
[go-server-source]: https://github.com/golang/go/blob/go1.27.0/src/net/http/server.go
