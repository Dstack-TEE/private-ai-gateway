# ACI clients

Use these clients when an application must verify the remote workload and its
channel before sending model request bytes. Choose the smallest layer that
matches your host.

## Choose a client

| Need | Use | What it owns |
| --- | --- | --- |
| Run a familiar curl command over an attested channel | [`aci curl`](../src/bin/aci/README.md#run-a-curl-request-over-the-verified-channel) | Service verification and TLS SPKI pinning, then delegates the request to system curl. |
| Verify one chat request, receipt, and cited session | [`aci send`](../docs/quickstart.md#5-verify-one-inference-end-to-end) | One complete command-line exchange and audit. |
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
`aci serve`.

## What every supported path enforces

The Rust and TypeScript clients share the same security meaning:

1. Fetch an attestation report with a fresh nonce.
2. Verify the TDX quote and the nonce-bound workload keyset.
3. Verify that the published compose is measured into RTMR3.
4. Reject an expired identity and any configured release-policy mismatch.
5. Send inference bytes only over TLS whose observed SPKI appears in the
   attested keyset.
6. Add verified-serving or session constraints before an aggregator forwards.
7. Capture exact wire digests and verify signed receipts and cited sessions
   when the selected client promises response verification.
8. Fail closed when a required check fails.

The browser verifier can check artifacts and quote evidence, but browser APIs
do not expose the peer certificate needed for SPKI pinning. Use the Node or Bun
runtime transport, the Rust CLI, or `aci serve` for a pinned channel.

## Verification versus release acceptance

Hardware verification and release acceptance are separate decisions:

- **Hardware-bound mode** proves that a genuine TDX workload owns the
  attested keys and reports the measured compose.
- **Reviewed-release mode** also requires the measured compose hash to appear
  in an operator-supplied allowlist.

`source_provenance.repo_url` and `repo_commit` are useful labels, but they are
not bound into the quote. The RTMR3-bound compose hash is the value clients can
pin. A branded production client should obtain accepted compose hashes through
an authenticated release channel.

The current Rust CLI and TypeScript verifier honestly skip the complete
private-key-custody policy. Review all skipped checks before sending sensitive
data.

## Coding-agent packages

The client workspace publishes eight packages:

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
