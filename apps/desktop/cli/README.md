# ACI Commands in Private AI Proxy

Command-line implementation of the
[ACI relying-party protocol](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/spec/aci.md).
The installed command is normally `pap`. It invokes the packaged
`private-ai-proxy` executable; the full name and the protocol-focused `aci`
alias expose the same commands.

```bash
cargo run --package private-ai-proxy --bin private-ai-proxy -- <command> --help
```

| Command | What it does |
| --- | --- |
| `verify <url>` | Fetch the attestation report with a fresh nonce and run the spec 9.1 identity checks; print the transcript. |
| `audit` | The same checks offline, over saved artifacts (report, receipt, bodies, session). |
| `sessions <url>` | Audit the service's current attested sessions (spec 9.2), optionally under a `--require-claim` policy. The accepted ids are what you pin (spec 5.3). |
| `send <url>` | One verified chat completion end to end: verify, send over the pinned channel, then verify the receipt and its cited session. |
| `serve <url>` | Local verifying proxy. Streams over the pinned channel, records each POST exchange's digests, and audits receipts after delivery. Receipt checks never delay response delivery. |

`serve --json-events` emits JSON Lines on stdout: `ready` after verification and the listener is bound,
`request_complete` for request activity and its optional `receipt_id`, `blocked` when forwarding fails
closed, `identity_updated` after successful re-verification, and `fatal` before
an unsuccessful exit. The `ready` event includes both `proxy_url` and
`control_url`. Human diagnostics remain on stderr.

Standalone `serve` also exposes a control listener (`--control`, default
`127.0.0.1:4183`, clear of the account callback on 4181 and the web UI on
4182) backed by the same bounded receipt store used for automatic
post-delivery audits. `GET /receipts` lists recent exchanges and
`POST /receipts/<id>/verify` retries one audit. Managed desktop mode does not
bind this listener because the backend owns the receipt store directly.

`serve` pins sessions two ways, both opt-in: `--session <id>` defines a
fixed accepted set, composed with request pins by intersection, and
`--require-claim <name[=source]>` derives the pin set from the audited
current sessions, refreshing it when the service refuses a superseded pin.

The TLS channel is pinned to every key the verified keyset attests for the
host. A keyset rotation blocks forwarding until a fresh verification passes:
a changed `X-ACI-Keyset-Digest` on a response triggers it, and so does a
handshake the pin refuses, since a rotated TLS key aborts the connection
before any response exists. A failed re-verification keeps the old pin.
After a successful one, the refused request is sent once more if the
identity it was admitted under still holds, and otherwise gets a retryable
503.

All five commands accept `--require-production-os`. Under that strict policy,
the client reads the RTMR3-bound `os-image-hash` and requires it to be in the
verifier's reviewed production-image allowlist. Development and unknown hashes
fail closed. Updating the allowlist requires a verifier release.

This option is an appraisal step, not a dstack boot verifier. The Proxy's ACI client
verifies the DCAP quote and replays RTMR3, but it does not reconstruct MRTD or
RTMR0-2 from the dstack OS image. Before relying on `policy-os: pass`, run a
dstack verifier over the same quote, event log, and VM configuration, and
require `is_valid: true`; that result establishes `os_image_hash` from those
boot measurements. See
[How the OS image is classified](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/providers/phala-direct/verification.md#how-the-os-image-is-classified).

## Where verification lives

The spec 9.1 checks run in the shared `aci-verifier` crate, the same
implementation Gateway uses to verify its upstreams: quote appraisal, the
§9.1(2) binding chain, provenance, custody, and §3.1 TLS selection. Receipt
signatures are checked in this package's
`aci/` modules. The neutral `aci-protocol` crate supplies wire types, JCS,
attestation-statement construction, and receipt canonicalization. The CLI maps
verification outcomes to a pass, fail, or honest skip and does not import a
gateway implementation.

Differences that are deliberate — the CLI's honest skips, and checks only a
relying party can run — are recorded in
[the ACI conformance notes](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/reviews/aci-spec-conformance-gaps.md).

[The ACI quickstart](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/quickstart.md) walks all of this against
a live deployment.
