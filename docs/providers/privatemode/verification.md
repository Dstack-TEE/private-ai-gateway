# Privatemode — co-deployed delegated attestation

- **TEE:** AMD SEV-SNP or Intel TDX + NVIDIA Confidential Computing
- **Session binding:** `manifest_image_sha256`: reviewed Contrast manifest,
  Coordinator policy, shared credential digest, and official proxy OCI image
  digest
- **Verifier:** official `privatemode-proxy` co-deployed in the gateway's
  measured dstack Compose
- **Transport:** private Compose HTTP to the proxy; Privatemode full-body E2EE
  from the proxy to model workers
- **Audit:** see [review.md](review.md)

## Trust boundary

Privatemode deliberately couples verification and encryption. The official
proxy verifies the Contrast Coordinator against a manifest, obtains the Mesh
CA, exchanges an inference secret with the Secret Service, and uses that secret
to encrypt inference bodies. Reimplementing only the quote check would not bind
gateway traffic to the secret released by that protocol.

The gateway therefore delegates this protocol to the official proxy, but it
does not run the proxy as a child process. dstack launches the gateway and proxy
as separate services in one measured Compose workload. That measurement binds
the proxy image digest, its command, the manifest mount, and the private network
topology. The proxy port is not published.
Its workspace is an unpersisted `tmpfs`, so a service restart cannot silently
reuse credential or Contrast state from an earlier container generation.
The proxy is launched with `--nvidiaOCSPAllowUnknown=false` and
`--nvidiaOCSPRevokedGracePeriod=0`; unknown or revoked NVIDIA certificate
status therefore fails closed instead of using the availability-oriented
upstream defaults.

The gateway's static config separately pins:

- the internal proxy origin;
- the exact manifest path and SHA-256 digest;
- the SHA-256 digest of the one accepted API credential;
- the official proxy OCI image digest recorded in the Compose file.

These fields cannot be changed through `PUT /v1/admin/upstreams`. A dynamic
Privatemode route is accepted only when its `base_url` exactly matches the
static origin; `bearer_token` is forbidden because the proxy owns provider
authentication, and `path` is forbidden because v1.48 has both encrypted and
unencrypted handlers. This prevents an admin-config update from redirecting
plaintext to a different proxy, injecting a second credential path, or selecting
an unencrypted proxy handler.

## Verification and forwarding

At startup, the gateway reads the mounted manifest and the same Compose secret
source mounted into the proxy. It verifies both measured digests, drops the
credential bytes, and requires exactly one Coordinator policy. When a route is
verified, the gateway sends an unauthenticated `GET /v1/models` to the pinned
internal origin using a client that ignores HTTP proxy environment variables.
The proxy started only after using its static `--apiKey` to complete Contrast
verification and inference-secret exchange; it attaches that credential to the
outbound model-list request itself. The gateway must not send an internal
Bearer: in v1.48 the proxy would ignore it once the static credential exists.
The client rejects redirects, so a forwarded prompt cannot leave the pinned
internal origin. The probe has an end-to-end deadline and rejects model-list
bodies over 1 MiB, including chunked responses without a declared length.

The verifier emits a verified event only after this probe returns a JSON model
list. The forwarding backend accepts only that exact manifest/image binding,
then sends OpenAI-compatible requests to the same internal origin. Independently
of config validation, it permits only the pinned v1.48 encrypted handler set:
`/v1/chat/completions`, `/v1/completions`, `/v1/embeddings`, and
`/v1/messages`. In particular, inference can never target the proxy's
unencrypted `/v1/models` forwarder. The proxy performs Privatemode full-body
encryption and response decryption.
Successful probes use the configured `verifier_cache_seconds` lease. Normal
requests reuse the binding until expiry, proactive refresh performs a fresh
probe, and invalidation discards the cached event.

Plain HTTP is intentional at this hop. It is not a remote trust channel: both
endpoints and their private network are inside the same attested dstack
workload. Adding a self-signed TLS layer would encrypt the same in-workload hop
without independently authenticating the measured service. The security
requirements are instead that the proxy remains in the measured Compose and
its port is never published.

## Configuration

Use [`deploy/compose.privatemode.yaml`](../../../deploy/compose.privatemode.yaml)
and set the reviewed manifest file and digest before deployment:

```bash
export PRIVATE_AI_GATEWAY_REPO_COMMIT=<audited-commit>
export PRIVATE_AI_GATEWAY_ADMIN_TOKEN=<admin-token>
export PRIVATE_AI_GATEWAY_ADMIN_TOKEN_SHA256="$(printf %s "$PRIVATE_AI_GATEWAY_ADMIN_TOKEN" | sha256sum | cut -d' ' -f1)"
export PRIVATE_AI_GATEWAY_INFERENCE_TOKEN=<long-random-client-token>
export PRIVATE_AI_GATEWAY_INFERENCE_TOKEN_SHA256="$(printf %s "$PRIVATE_AI_GATEWAY_INFERENCE_TOKEN" | sha256sum | cut -d' ' -f1)"
export PRIVATEMODE_API_KEY=<privatemode-api-key>
export PRIVATEMODE_MANIFEST_PATH=/absolute/path/to/exact-reviewed-manifest.json

deploy/render-privatemode-compose.sh /tmp/private-ai-gateway-privatemode.json
phala deploy -n private-ai-gateway \
  -c /tmp/private-ai-gateway-privatemode.json \
  -e PRIVATE_AI_GATEWAY_ADMIN_TOKEN="$PRIVATE_AI_GATEWAY_ADMIN_TOKEN" \
  -e PRIVATEMODE_API_KEY="$PRIVATEMODE_API_KEY"
```

Rendering makes the exact manifest bytes and non-secret pins part of the
measured Compose. The renderer verifies that inline serialization preserves the
manifest file's SHA-256, including its whitespace and final newline.
The admin and Privatemode secrets remain outside it and enter only through the
encrypted deployment environment. The renderer derives the credential digest
from `PRIVATEMODE_API_KEY`; Compose mounts one secret source into the gateway
and the proxy. The gateway verifies the actual mounted bytes against that
measured digest at startup, while the proxy consumes the same source through
`--apiKey @<file>`. The downstream inference token never enters the deployment:
only its digest is measured, and clients present the token as a Bearer
credential on every inference request.

The measured static gateway config has this shape:

```json
{
  "inference_token_sha256": "<sha256-of-high-entropy-client-bearer>",
  "privatemode_proxy": {
    "base_url": "http://privatemode-proxy:8080",
    "manifest_path": "/run/privatemode/manifest.json",
    "manifest_sha256": "<64-lowercase-hex-characters>",
    "credential_path": "/run/secrets/privatemode-api-key",
    "credential_sha256": "<sha256-of-privatemode-api-key>",
    "proxy_image_digest": "sha256:ff900b263a51a437633d15da809e7893a31fa4b1f4acfa4e526c075682d84307"
  }
}
```

Configure the mutable model route after boot:

```json
[
  {
    "name": "privatemode",
    "provider": "privatemode",
    "base_url": "http://privatemode-proxy:8080",
    "models": {
      "gpt-oss-120b-private": "gpt-oss-120b"
    }
  }
]
```

One co-deployed proxy supports one gateway upstream entry. Put all models that
share its credential in that entry. The official proxy loads one credential
from its Compose secret file at startup, and the gateway validates that same
mounted source against static `credential_sha256`. To rotate the key, rerender
with the new `PRIVATEMODE_API_KEY` and redeploy both services. If distinct
credentials are required, deploy distinct measured proxy services rather than
pointing multiple entries at one service.

## Session binding

The verifier emits one binding:

```json
{
  "type": "manifest_image_sha256",
  "provider": "privatemode",
  "manifest_sha256": "...",
  "coordinator_policy_hash": "...",
  "proxy_image_digest": "sha256:...",
  "credential_sha256": "..."
}
```

The event's `url_origin` is the pinned internal service origin. Its verifier id
is `privatemode-proxy/co-deployed-contrast/v1`. The exact manifest bytes are
retained as verification evidence.

## Failure behavior

The route fails closed when static proxy policy is absent; the route origin
differs from the static origin; the manifest is missing, malformed, or has the
wrong digest; the mounted credential is missing, malformed, or differs from its
measured digest; the manifest does not contain exactly one Coordinator policy;
the proxy image digest is malformed; mutable config supplies a bearer or path;
the model-list probe fails; forwarding targets a handler outside the encrypted
allowlist; or the verified receipt binding differs from the active deployment.

There is no fallback to the public Privatemode API, an operator-supplied remote
proxy, an HTTP redirect, or an HTTP proxy from the process environment. Proxy
error bodies are not exposed in public verification failures. Container
restart and lifecycle policy belong to Compose.

## Updates

A manifest or proxy-image change alters the channel TCB. Review both changes,
update the manifest digest and image digest in the measured static deployment,
and redeploy. A dynamic upstream-config replacement cannot mutate these pins.

## Sources

- [Privatemode attestation overview](https://docs.privatemode.ai/architecture/attestation/overview/)
- [Privatemode proxy configuration](https://docs.privatemode.ai/api/proxy-configuration/)
- [Privatemode TCB source](https://github.com/edgelesssys/privatemode-public)
- [Contrast source](https://github.com/edgelesssys/contrast)
