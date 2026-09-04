# Private AI Gateway documentation

Use this index to choose the document that matches your task. The ACI protocol
itself lives under [`spec/`](../spec/README.md); this directory documents the
reference gateway, its deployment, and its provider integrations.

## Start here

| Reader | Goal | Document | Type |
| --- | --- | --- | --- |
| Evaluator | Verify a live ACI deployment | [ACI quickstart](quickstart.md) | Tutorial |
| Evaluator | Understand the security claim and its limits | [ACI verification and security model](attested-confidential-inference.md) | Explanation |
| Client implementer | Choose the Rust CLI, TypeScript transport, or shared provider | [ACI clients](../clients/README.md) | Index |
| Coding-agent user | Use ACI natively from Pi or OpenCode | [Coding-agent integrations](../clients/coding-agents.md) | How-to |
| Developer | Run and change the gateway locally | [Local development](getting-started.md) | Tutorial |
| Operator | Configure runtime policy and upstreams | [Configuration reference](configuration-reference.md) | Reference |
| Client implementer | Call inference and artifact endpoints | [HTTP API reference](api-reference.md) | Reference |
| Control-plane implementer | Supply authorization, routing, catalogs, and usage ingestion | [Control-plane contract](control-plane-contract.md) | Reference |
| Operator | Deploy the gateway in dstack | [git-launcher deployment](../deploy/README.md) | How-to |
| Maintainer | Run unit, smoke, and live-provider tests | [Testing guide](live-e2e-test-suite.md) | How-to |
| Contributor | Prepare and validate a change | [Contributing](../CONTRIBUTING.md) | How-to and policy |
| Auditor | Review provider-specific evidence and policy | [Provider verification index](providers/README.md) | Reference |

## ACI artifacts and lifecycle

| Topic | Document | Purpose |
| --- | --- | --- |
| Attestation, receipts, E2EE, and end-to-end verification | [ACI verification and security model](attested-confidential-inference.md) | Defines what a relying party must verify. |
| Attested-session records | [Attested sessions](attested-session-system.md) | Explains session IDs, claims, evidence, storage, and receipt linkage. |
| Verification leases and Chutes nonce sessions | [Upstream verification lifecycle](upstream-verification-lifecycle.md) | Explains cached verification and provider-session state. |
| Provider admission | [Provider audit criteria](providers/audit-criteria.md) | Defines the questions a provider integration must answer. |

## Maintainer records

The following documents record plans or point-in-time reviews. They can explain
why code exists, but they are not runtime references:

- [Project status and roadmap](roadmap.md)
- [Router-mode provider review process](router-mode-provider-review.md)
- [ACI implementation gap review](reviews/aci-spec-conformance-gaps.md)
- [Router soundness review](reviews/router-mode-soundness.md)
- [Router load-balancing and cache review](reviews/router-mode-load-balancing-cache.md)
- each provider's dated `review.md` under [`providers/`](providers/README.md)

Use the source files named in each living reference when a review record and the
current implementation disagree.

## Documentation conventions

- `spec/aci.md` is normative for the protocol. Implementation docs describe the
  behavior of this repository.
- Provider `verification.md` files track the current adapter. Provider
  `review.md` files are dated admission records.
- Examples use the canonical `/v1/aci/*` artifact endpoints. The
  `/v1/attestation/report` and `/v1/signature/{id}` routes exist for legacy
  dstack-vllm-proxy clients.
- Security statements identify the policy or request constraint that makes the
  gateway fail closed. A provider name alone does not imply enforcement.
