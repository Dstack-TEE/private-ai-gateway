# Contributing

Private AI Gateway is a security-sensitive reference implementation. Changes to routing, canonical JSON, attestation, key custody, receipt signing, E2EE, or channel binding need tests that show both the accepted case and the tampered or rejected case.

## Development setup

[Local development](docs/getting-started.md) covers prerequisites, dependency installation, the dstack SDK socket, and a first gateway run.

## Required checks

Before opening a change, run the [repository checks](docs/getting-started.md#run-the-repository-checks). Run the TypeScript client checks listed there as well when you change `clients/` or a shared ACI construction.

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
5. Update the provider's `verification.md` page in the same change.
6. Run the relevant hermetic and live tests.

The provider's `verification.md` page must state:

- the evidence endpoints and freshness mechanism;
- every mandatory rejection check;
- the exact value bound into hardware evidence;
- how forwarding enforces that value;
- typed claims and their sources;
- evidence or checks that are supplemental only;
- tests and a repository-relative reproduction command;
- limitations that affect a relying-party decision.

Do not rewrite a dated `review.md` to match new code. Add a short dated status note instead.

### Configuration and APIs

Unknown JSON fields are rejected deliberately. A new setting requires:

- a typed field and validation;
- a documented default and zero or empty-value behavior;
- redaction when it can contain a secret;
- serialization and replacement tests.

### Documentation

Update the living page that owns a behavior in the same change as the code:

| Change | Page to update |
| --- | --- |
| Routes or wire behavior | [HTTP API reference](docs/api-reference.md) |
| Fields and defaults | [Configuration reference](docs/configuration-reference.md) |
| Middleware decisions | [Control-plane contract](docs/control-plane-contract.md) |
| Verifier claims and limitations | The provider's `verification.md` under [`docs/providers/`](docs/providers/README.md) |
| Runnable validation | [Live end-to-end suite](docs/live-e2e-test-suite.md) |

## Documentation standard

Living documentation must describe the current code, not an intended design. Follow the [documentation conventions](docs/README.md#documentation-conventions), and:

- Put setup and learning sequences in tutorials.
- Put runnable tasks in how-to guides.
- Put fields, defaults, routes, and schemas in references.
- Put trust boundaries and design reasoning in explanations.
- Label point-in-time observations with a date and reviewed revision.
- Call out what a verifier does not prove.
- Use repository-relative commands and links.
- Remove placeholders and private workstation paths.

After changing Markdown, check local links and search for stale source paths or renamed fields.

## Commit scope

Keep generated output, local `.env` files, provider credentials, live evidence, and temporary state out of commits. Do not update provider pins from an unreviewed first observation. Explain security-relevant behavior changes in the commit or pull-request description, including the new failure behavior and tests that cover it.
