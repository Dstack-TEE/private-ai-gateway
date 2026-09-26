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
| `curl <https-url> -- [options]` | Verify the service, then run system curl for one request with the attested TLS key pinned. |
| `serve <url>` | Local verifying proxy. Streams over the pinned channel, records each POST exchange's digests, and audits receipts after delivery. Receipt checks never delay response delivery. |

`serve --json-events` emits JSON Lines on stdout: `ready` after verification and the listener is bound,
`request_complete` for request activity and its optional `receipt_id`, `blocked` when forwarding fails
closed, `identity_updated` after successful re-verification, and `fatal` before
an unsuccessful exit. The `ready` event includes both `proxy_url` and
`control_url`. Human diagnostics remain on stderr.

Standalone `serve` also exposes a control listener (`--control`, default
`127.0.0.1:4183`, clear of the web UI on 4182) backed by the same bounded receipt store used for automatic
post-delivery audits. `GET /receipts` lists recent exchanges and
`POST /receipts/<id>/verify` retries one audit. Managed desktop mode does not
bind this listener because the backend owns the receipt store directly.

`serve` pins sessions two ways, both opt-in: `--session <id>` defines a
fixed accepted set, composed with request pins by intersection, and
`--require-claim <name[=source]>` derives the pin set from the audited
current sessions, refreshing it when the service refuses a superseded pin.

The TLS channel is pinned to the keys the verified report declares for the
host: every attested TLS key when none is domain-scoped, otherwise only the
entry its `downstream_tls_binding` names (spec 4.2). A keyset rotation blocks
forwarding until a fresh verification passes: a changed `X-ACI-Keyset-Digest`
on a response triggers it, and so does a handshake the pin refuses, since a
rotated TLS key aborts the connection before any response exists. A failed
re-verification keeps the old pin. After a successful one, the refused
request is sent once more if the identity it was admitted under still holds,
and otherwise gets a retryable 503.

Custody (id-5) is checked when a custody policy is given: repeatable
`--accept-dstack-kms-root-public-key` together with repeatable
`--accept-subject app-id:0x<hex>`, the measured dstack app-id. The receipt
key's dstack KMS signature chain must then end at an accepted root, anchored
on the app-id the verified event log measures. The report's self-asserted
`image_digest` is never an anchor: nothing measured corroborates it (spec
4.1), so any app under the same KMS root could claim it. Without a policy
id-5 is an honest skip; no trust anchors are built in. The flags sit beside
`--accept-compose` on `verify`, `audit`, `sessions`, `send`, `curl` and `serve`, and
`serve`'s `ready` event reports them under `policy`.

All six commands accept `--require-production-os`. Under that strict policy,
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

Every relying-party verification step lives in this package's `aci/` modules:
quote appraisal, the §9.1(2) binding chain, §3.1 TLS selection, and receipt
signatures. When the keyset has domain-scoped TLS keys, the channel check and
the pin use only the entry the report's `downstream_tls_binding` declares
(§4.2), the same selection Gateway makes for its upstreams. The neutral
`aci-protocol` crate supplies only wire types, JCS, attestation-statement
construction, and receipt canonicalization. The CLI maps verification
outcomes to a pass, fail, or honest skip and does not import a gateway
implementation.

Differences that are deliberate — the CLI's honest skips, and checks only a
relying party can run — are recorded in
[the ACI conformance notes](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/reviews/aci-spec-conformance-gaps.md).

[The ACI quickstart](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/quickstart.md) walks all of this against
a live deployment.
