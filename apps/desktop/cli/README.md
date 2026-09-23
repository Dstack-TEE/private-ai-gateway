# ACI Commands in Private AI Proxy

Command-line implementation of the
[ACI relying-party protocol](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/spec/aci.md).
The installed command is normally `pap`. It invokes the packaged
`private-ai-proxy` executable; the full name and the protocol-focused `aci`
alias expose the same commands.

```bash
cargo run --manifest-path cli/Cargo.toml --bin private-ai-proxy -- <command> --help
```

| Command | What it does |
| --- | --- |
| `verify <url>` | Fetch the attestation report with a fresh nonce and run the spec 9.1 identity checks; print the transcript. |
| `audit` | The same checks offline, over saved artifacts (report, receipt, bodies, session). |
| `sessions <url>` | Audit the service's current attested sessions (spec 9.2), optionally under a `--require-claim` policy. The accepted ids are what you pin (spec 5.3). |
| `send <url>` | One verified chat completion end to end: verify, send over the pinned channel, then verify the receipt and its cited session. |
| `serve <url>` | Local verifying proxy. Streams over the pinned channel, records each POST exchange's digests, and audits receipts after delivery. On-demand retries use the control endpoint (default `127.0.0.1:4181`). Receipt checks never delay response delivery. |

`serve --json-events` is the desktop/process-integration mode. Its stdout is
JSON Lines only: `ready` after verification and both listeners are bound,
`request_complete` for request activity and its optional `receipt_id`, `blocked` when forwarding fails
closed, `identity_updated` after successful re-verification, and `fatal` before
an unsuccessful exit. Human diagnostics remain on stderr.

`serve` pins sessions two ways, both opt-in: `--session <id>` defines a
fixed accepted set, composed with request pins by intersection, and
`--require-claim <name[=source]>` derives the pin set from the audited
current sessions, refreshing it when the service refuses a superseded pin.

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

Relying-party policy and appraisal live in this package's `aci/` modules. The
neutral `aci-protocol` crate supplies wire types, JCS, attestation-statement
construction, and receipt canonicalization; `aci-verify` supplies policy-neutral
DCAP, binding-chain, dstack measurement, and TLS SPKI mechanisms. The CLI maps
those mechanisms to a pass, fail, or honest skip under its own policy and does
not import a gateway implementation.

Differences that are deliberate — the CLI's honest skips, and checks only a
relying party can run — are recorded in
[the ACI conformance notes](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/reviews/aci-spec-conformance-gaps.md).

[The ACI quickstart](https://github.com/Dstack-TEE/private-ai-gateway/blob/main/docs/quickstart.md) walks all of this against
a live deployment.
