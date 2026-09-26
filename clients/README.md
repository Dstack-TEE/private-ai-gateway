# ACI clients

Use these clients when an application must verify the remote workload and its
channel before sending model request bytes. Choose the smallest layer that
matches your host.

## Choose a client

| Need | Use | What it owns |
| --- | --- | --- |
| Run a familiar curl request over an attested channel | [`pap curl`](../apps/desktop/docs/cli.md#one-pinned-curl-request) | Service verification and TLS SPKI pinning, then delegates the request to system curl. |
| Verify one chat request, receipt, and cited session | [`pap send`](../docs/quickstart.md#5-verify-one-inference-end-to-end) | One complete command-line exchange and audit. |
| Verify artifacts in a browser, or add pinned fetch to Node or Bun | [`@phala/aci-verifier`](verifier-ts/README.md) | Reports, quotes, measurements, pinned runtime transport, wire digests, receipts, and sessions. |
| Build a provider adapter for another host | [`@phala/aci-provider`](provider/README.md) | Verified connection lifecycle, model catalog, filtering, receipt history, response verification, and inspection. |
| Use ACI in Pi | [`@phala/pi-provider-aci`](pi-provider/README.md) or a branded package | Native Pi auth, model picker, persistence, commands, and fail-closed receipt verification. |
| Use ACI in OpenCode | [`@phala/opencode-provider-aci`](opencode-provider/README.md) or a branded package | Native OpenCode provider, auth, catalog, tools, lifecycle, and fail-closed receipt verification. |

If your SDK accepts a custom `fetch`, start with `connectAci()` from
`@phala/aci-verifier/runtime`. If you are integrating a coding agent or another
host with its own provider lifecycle, use `@phala/aci-provider` and map its
host-neutral contracts into the host's official APIs.

A base URL alone cannot install an attested TLS transport. A host needs a
custom-fetch hook, a provider-plugin boundary, or a local proxy such as
`pap serve`.

## Shared checks and client-specific policy

Pinned Rust and TypeScript inference transports share these checks:

1. Fetch an attestation report with a fresh nonce.
2. Verify the TDX quote and the nonce-bound workload keyset. The TypeScript
   verifier reports the TCB status without enforcing it.
3. Verify that the published compose is measured into RTMR3.
4. Reject an expired identity and any configured release-policy mismatch.
5. Send inference bytes only over TLS whose observed SPKI is an attested TLS
   key for the host. The Rust CLI accepts only the entry the report declares in
   `downstream_tls_binding` when the keyset scopes keys to domains. The
   TypeScript runtime accepts any attested key for the host.
6. Fail closed when a required check fails.

Serving and response policy then depends on the client. `pap send`, `pap serve`,
and the TypeScript runtime require verified serving by default. The TypeScript
runtime applies this to JSON POST requests. `pap curl` passes the caller's
request body unchanged, so include `provider.aci_verified` or session IDs when
the request must fail closed at the provider hop. Clients
that promise response verification capture exact wire digests and verify signed
receipts and cited sessions; `pap curl` does not verify a response receipt.

The browser verifier can check artifacts and quote evidence, but browser APIs
do not expose the peer certificate needed for SPKI pinning. Use the Node or Bun
runtime transport, the Rust CLI, or `pap serve` for a pinned channel.

## Verification versus release acceptance

Hardware verification and release acceptance are separate decisions:

- **Hardware-bound mode** proves that a genuine TDX workload booted the
  measured compose and serves the attested keys.
- **Reviewed-release mode** also requires the measured compose hash to appear
  in an accepted list: `--accept-compose` in Rust, `acceptedComposeHashes` in
  TypeScript.

The compose hash is the value to pin. The report's `source_provenance` is the
gateway's own statement. Read the repository and commit from the measured
compose instead.

The Pi and OpenCode packages, including the RedPill and Phala Cloud
distributions, run in hardware-bound mode: they ship no accepted compose
hashes. Users can supply them through the `trust.acceptedComposeHashes` option
or the `REDPILL_ACCEPTED_COMPOSE_HASHES` or `PHALA_ACCEPTED_COMPOSE_HASHES`
environment variable. Shipping
reviewed hashes through an authenticated release channel is planned.

Key custody is also established by review. The Rust CLI checks custody of the
receipt-signing key when given `--accept-subject` and
`--accept-dstack-kms-root-public-key`. The TypeScript verifier does not check
custody. No client can check where the TLS private key lives: that follows
from reviewing the compose that runs the TLS terminator.

## Coding-agent packages

Besides `@phala/aci-verifier`, the client workspace publishes these packages:

| Host | Neutral package | RedPill | Phala Cloud |
| --- | --- | --- | --- |
| Shared kernel | [`@phala/aci-provider`](provider/README.md) | Shared profile | Shared profile and account flow |
| Pi | [`@phala/pi-provider-aci`](pi-provider/packages/pi-provider-aci/README.md) | [`pi-provider-redpill`](pi-provider/packages/pi-provider-redpill/README.md) | [`pi-provider-phala-cloud`](pi-provider/packages/pi-provider-phala-cloud/README.md) |
| OpenCode | [`@phala/opencode-provider-aci`](opencode-provider/packages/opencode-provider-aci/README.md) | [`opencode-provider-redpill`](opencode-provider/packages/opencode-provider-redpill/README.md) | [`opencode-provider-phala-cloud`](opencode-provider/packages/opencode-provider-phala-cloud/README.md) |

The branded packages are thin distributions over the same verifier and
provider kernel. They add product identity, default endpoints, environment
names, and authentication options. They do not implement separate verification
logic.

See [Coding-agent integrations](coding-agents.md) for installation and host
behavior, [Client architecture](architecture.md) for component ownership, and
[Releasing](releasing.md) for the coordinated npm release process.

## Trust boundary

The local application or coding agent sees plaintext prompts and responses.
These clients protect the remote model HTTP path. MCP servers, tools, browser
automation, shell commands, WebSockets, extensions, and host telemetry have
separate trust boundaries.

Provider authentication is also outside ACI. RedPill packages accept API keys.
Phala Cloud packages additionally expose the shared device authorization flow.
Pi and OpenCode persist credentials through their own native stores; the ACI
packages do not create a parallel credential database.
