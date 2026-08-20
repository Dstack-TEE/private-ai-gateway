# Contributing

Private AI Gateway is a security-sensitive reference implementation. Changes to routing, canonical JSON, attestation, key custody, receipt signing, E2EE, or channel binding need tests that show both the accepted case and the tampered or rejected case.

## Development setup

Install:

- Rust stable with `rustfmt` and `clippy`;
- Python 3.12;
- [uv](https://docs.astral.sh/uv/);
- Node.js 20 when changing the TypeScript verifier.

From the repository root:

```sh
uv sync --locked
cargo build --all-targets
```

A running gateway also needs a dstack SDK endpoint. See [Local development](docs/getting-started.md) for the socket and startup flow.

## Required checks

Run the main CI checks before opening a change:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
python3 -m compileall scripts
```

When `clients/verifier-ts/` or an ACI construction changes:

```sh
cd clients/verifier-ts
npm ci
npm run build
npm test
```

Live-provider tests consume credentials and quota, so they are not part of credential-free CI. Use [Run the live end-to-end suite](docs/live-e2e-test-suite.md) for provider adapter, attestation, and forwarding changes.

## Change requirements

### ACI wire or cryptography

Update the specification, implementation, and test vectors together when a change affects:

- JCS input or field names;
- workload-keyset, report-data, session, or receipt digests;
- signature payloads or algorithms;
- E2EE keys, AAD, nonces, or encrypted field paths;
- receipt events, session claims, or evidence encoding;
- keyset expiry and rotation behavior.

Do not add a compatibility shortcut to the canonical `/v1/aci/*` artifacts. Keep legacy behavior on the explicitly documented legacy routes and test the two surfaces separately.

### Provider verification

A provider verifier must return an enforceable channel binding. A successful evidence check without transport enforcement is not an accepted integration.

For a new or changed provider:

1. Apply [Provider audit criteria](docs/providers/audit-criteria.md).
2. Add negative tests for changed nonce, quote, binding, measurement, or pin as applicable.
3. Keep supplemental evidence separate from mandatory gates.
4. Map claims to `asserted`, `refuted`, or `unknown` without upgrading missing evidence.
5. Update the provider's living `verification.md` page.
6. Run the relevant hermetic and live tests.

Preserve a dated `review.md` as an audit record. Add a supersession note when the implementation changes rather than rewriting what the audit observed.

### Configuration and APIs

Unknown JSON fields are rejected deliberately. A new setting requires:

- a typed field and validation;
- a documented default and zero or empty-value behavior;
- redaction when it can contain a secret;
- serialization and replacement tests;
- an update to [Configuration reference](docs/configuration-reference.md).

A route or wire-behavior change requires an update to [HTTP API reference](docs/api-reference.md). Middleware request or response changes also require [Control-plane contract](docs/control-plane-contract.md) updates.

## Documentation standard

Living documentation must describe the current code, not an intended design.

- Put setup and learning sequences in tutorials.
- Put runnable tasks in how-to guides.
- Put fields, defaults, routes, and schemas in references.
- Put trust boundaries and design reasoning in explanations.
- Label point-in-time observations with a date and reviewed revision.
- Use canonical `/v1/aci/*` routes in new examples.
- State the condition that makes a security property fail closed.
- Call out what a verifier does not prove.
- Use repository-relative commands and links.
- Remove placeholders and private workstation paths.

Start at the [Documentation index](docs/README.md). After changing Markdown, check local links and search for stale source paths or renamed fields.

## Commit scope

Keep generated output, local `.env` files, provider credentials, live evidence, and temporary state out of commits. Do not update provider pins from an unreviewed first observation. Explain security-relevant behavior changes in the commit or pull-request description, including the new failure behavior and tests that cover it.
