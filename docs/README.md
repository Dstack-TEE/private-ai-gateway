# Private AI Gateway documentation

Use this index to choose the document that matches your task. The ACI protocol
itself lives under [`spec/`](../spec/README.md); this directory documents the
reference gateway, its deployment, and its provider integrations.

## Use an ACI service

For developers and evaluators who call a deployment or build on the clients.

| Goal | Document | Type |
| --- | --- | --- |
| Verify a live deployment and send a verified request | [ACI quickstart](quickstart.md) | Tutorial |
| Install the Private AI Proxy desktop app or `pap` CLI | [Private AI Proxy install](private-ai-proxy-install.md) | How-to |
| Choose a client library, CLI command, or integration | [ACI clients](../clients/README.md) | Index |
| Use private inference from Pi or OpenCode | [Coding-agent integrations](../clients/coding-agents.md) | How-to |
| Call inference and artifact endpoints | [HTTP API reference](api-reference.md) | Reference |

## Understand and audit the security claim

For anyone deciding whether to trust a deployment or reviewing a provider.

| Goal | Document | Type |
| --- | --- | --- |
| Understand the privacy claim, what to verify, and its limits | [Verification and security model](attested-confidential-inference.md) | Explanation |
| See what each provider verifier proves | [Provider verification index](providers/README.md) | Reference |
| Understand attested-session records and claims | [Attested sessions](attested-session-system.md) | Explanation |
| Understand verification caching and Chutes nonce sessions | [Upstream verification lifecycle](upstream-verification-lifecycle.md) | Explanation |
| Admit a new provider | [Provider audit criteria](providers/audit-criteria.md) | Reference |

## Run or change the gateway

For operators and contributors.

| Goal | Document | Type |
| --- | --- | --- |
| Build and run the gateway locally | [Local development](getting-started.md) | Tutorial |
| Deploy the gateway in dstack | [git-launcher deployment](../deploy/README.md) | How-to |
| Deploy the gateway with the Privatemode proxy | [Privatemode deployment](../deploy/privatemode.md) | How-to |
| Configure runtime policy and upstreams | [Configuration reference](configuration-reference.md) | Reference |
| Implement a control plane | [Control-plane contract](control-plane-contract.md) | Reference |
| Run unit, smoke, and live-provider tests | [Testing guide](live-e2e-test-suite.md) | How-to |
| Prepare and validate a change | [Contributing](../CONTRIBUTING.md) | How-to and policy |

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
- Each fact has one home page. Other pages link to it instead of restating it.
