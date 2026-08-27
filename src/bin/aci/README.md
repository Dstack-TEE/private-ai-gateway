# `aci` CLI

The reference command-line client for the ACI protocol
([spec/aci.md](../../../spec/aci.md)). It reuses the gateway's own
verification code and fails closed: exit code 0 means VERIFIED.

```bash
cargo run --bin aci -- <command> --help
```

| Command | What it does |
| --- | --- |
| `verify <url>` | Fetch the attestation report with a fresh nonce and run the spec 9.1 identity checks; print the transcript. |
| `audit` | The same checks offline, over saved artifacts (report, receipt, bodies, session). |
| `sessions <url>` | Audit the service's current attested sessions (spec 9.2), optionally under a `--require-claim` policy. The accepted ids are what you pin (spec 5.3). |
| `send <url>` | One verified chat completion end to end: verify, send over the pinned channel, then verify the receipt and its cited session. |
| `serve <url>` | Local verifying proxy. Forwards every method and path over the pinned channel, records each POST exchange's digests, and verifies receipts on demand from a control endpoint (default `127.0.0.1:4181`). |

`serve` pins sessions two ways, both opt-in: `--session <id>` defines a
fixed accepted set, composed with request pins by intersection, and
`--require-claim <name[=source]>` derives the pin set from the audited
current sessions, refreshing it when the service refuses a superseded pin.

All five commands accept `--require-production-os`. Under that strict policy,
the client reads the RTMR3-bound `os-image-hash` and requires it to be in the
verifier's reviewed production-image allowlist. Development and unknown hashes
fail closed. Updating the allowlist requires a verifier release.

This option is an appraisal step, not a dstack boot verifier. The `aci` client
verifies the DCAP quote and replays RTMR3, but it does not reconstruct MRTD or
RTMR0-2 from the dstack OS image. Before relying on `policy-os: pass`, run a
dstack verifier over the same quote, event log, and VM configuration, and
require `is_valid: true`; that result establishes `os_image_hash` from those
boot measurements. See [How the OS image is classified](../../../docs/providers/phala-direct/verification.md#how-the-os-image-is-classified).

## Where verification lives

Every verification step is `src/aci`'s, so the gateway's own upstream
verifier and this CLI run the same code — the quote steps, the §9.1(2)
binding chain, the §3.1 TLS selection, receipt signatures, JCS digests. What
lives here is the transcript: mapping each step's outcome to a pass, fail, or
honest skip. Two implementations of one step drift, and both keep passing
their own tests while disagreeing about the same service, so
`tests/layering.rs` fails the build if this CLI reaches for a verification
primitive directly.

Differences that are deliberate — the CLI's honest skips, and checks only a
relying party can run — are recorded in
[docs/reviews/aci-spec-conformance-gaps.md](../../../docs/reviews/aci-spec-conformance-gaps.md).

[docs/quickstart.md](../../../docs/quickstart.md) walks all of this against
a live deployment.
