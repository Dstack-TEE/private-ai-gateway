# Private AI Gateway documentation

This index lists every guide and reference in the repository, grouped by what
you are trying to do. It covers the gateway, the Private AI Proxy client, and
the provider integrations; the ACI protocol itself has its own
[specification index](../spec/README.md).

New here? Run the [ACI quickstart](quickstart.md), then read the
[verification and security model](attested-confidential-inference.md).

## Call an ACI service

For developers and evaluators who send requests to a deployment.

| Goal | Document | Type |
| --- | --- | --- |
| Verify a live deployment and send a verified request | [ACI quickstart](quickstart.md) | Tutorial |
| Install the `pap` CLI or the desktop app | [Install Private AI Proxy](private-ai-proxy-install.md) | How-to |
| Choose between `pap`, the desktop app, and the TypeScript verifier | [ACI clients](../clients/README.md) | Index |
| Verify artifacts in a browser, or pin `fetch` in a Node or Bun SDK | [`@phala/aci-verifier`](../clients/verifier-ts/README.md) | Reference |
| Look up a route, its streaming and E2EE support, and its errors | [HTTP API reference](api-reference.md) | Reference |
| Encrypt requests end to end, or move a client off the legacy E2EE scheme | [Migrate to E2EE v2.1](e2ee-v2.1-migration.md) | How-to |

## Use Private AI Proxy

For people running coding agents through the local verifying client.

| Goal | Document | Type |
| --- | --- | --- |
| Learn what the app and CLI do and how a request flows | [Private AI Proxy](../apps/desktop/README.md) | Overview |
| Connect coding agents, run the local proxy, and script `pap` | [CLI reference](../apps/desktop/docs/cli.md) | Reference |
| Find and edit settings, profiles, and stored credentials | [Settings files](../apps/desktop/docs/configuration.md) | Reference |
| Sign in with a Phala or RedPill account | [Account login](../apps/desktop/docs/account-login.md) | Reference |
| See what changed in each release | [Changelog](../apps/desktop/CHANGELOG.md) | Reference |

## Understand and audit the security claim

For anyone deciding whether to trust a deployment or reviewing a provider.

| Goal | Document | Type |
| --- | --- | --- |
| Understand the privacy claim, what to verify, and its limits | [Verification and security model](attested-confidential-inference.md) | Explanation |
| Read or implement the ACI protocol | [ACI specification index](../spec/README.md) | Index |
| See what each provider verifier proves, and its audit decision | [Provider verification index](providers/README.md) | Index |
| Understand attested-session records and their claims | [Attested sessions](attested-session-system.md) | Explanation |
| Understand verification caching, refresh, and binding failures | [Upstream verification lifecycle](upstream-verification-lifecycle.md) | Explanation |
| Check how prompt caches are partitioned per user | [Prefix-cache isolation](prefix-cache-isolation.md) | Explanation |
| Admit a new provider | [Provider audit criteria](providers/audit-criteria.md) | Reference |

## Run the gateway

For operators deploying and configuring a gateway.

| Goal | Document | Type |
| --- | --- | --- |
| Build the gateway, run the repository checks, and start it locally | [Local development](local-development.md) | Tutorial |
| Configure runtime policy and upstreams | [Configuration reference](configuration-reference.md) | Reference |
| Deploy the gateway in dstack | [Deploy with git-launcher](../deploy/README.md) | How-to |
| Extract release values and monitor deployment drift | [Gateway release measurements](gateway-measurements.md) | How-to and reference |
| Deploy the gateway with the Privatemode proxy | [Deploy with the Privatemode proxy](../deploy/privatemode.md) | How-to |
| Add a private Chutes route | [Configure a private Chutes route](providers/chutes/configuration.md) | How-to |
| Add an audio-transcription upstream | [Audio transcription example](../examples/audio/README.md) | Example |
| Implement a control plane for middleware mode | [Control-plane contract](control-plane-contract.md) | Reference |
| Start from a working control plane | [Minimal control plane](../examples/control-plane/README.md) | Example |
| Run the smoke tests and the live-provider suite | [Live end-to-end suite](live-e2e-test-suite.md) | How-to |
| Test per-user cache salts on a disposable CVM | [Cache-salt CVM test](cache-salt-cvm-test.md) | How-to |

## Change the code

For contributors.

| Goal | Document | Type |
| --- | --- | --- |
| Prepare and validate a change, and know which page to update | [Contributing](../CONTRIBUTING.md) | How-to and policy |
| Understand the TypeScript verifier and its trust contract | [ACI client architecture](../clients/architecture.md) | Explanation |
| Understand the Private AI Proxy components and their boundaries | [Private AI Proxy architecture](../apps/desktop/docs/client-architecture.md) | Explanation |
| Find the `pap` crate and where its verification lives | [`private-ai-proxy` crate](../apps/desktop/cli/README.md) | Reference |
| Keep streaming wire bytes unchanged during a refactor | [Streaming wire characterization](../tests/streaming/README.md) | Reference |

## Release Private AI Proxy and the clients

For maintainers shipping releases.

| Goal | Document | Type |
| --- | --- | --- |
| Understand release orchestration and how each installation updates | [Distribution architecture](../apps/desktop/docs/distribution.md) | Explanation |
| Know what each CLI package contains and the release contract | [CLI distribution](../apps/desktop/docs/cli-distribution.md) | Reference |
| Publish the CLI to npm | [npm distribution](../apps/desktop/npm/README.md) | Reference |
| Prepare and submit a Mac App Store build | [Mac App Store](../apps/desktop/docs/mac-app-store.md) | How-to |
| Refresh the bundled Codex model catalog | [Codex catalog](../apps/desktop/docs/codex-catalog.md) | How-to |
| Run the desktop live end-to-end test | [Desktop live E2E](../apps/desktop/e2e/README.md) | How-to |
| Release `@phala/aci-verifier` to npm | [Verifier releases](../clients/releasing.md) | How-to |

## Maintainer records

These documents record plans or point-in-time reviews. They explain why code
exists, but they are not runtime references:

- [Project status and roadmap](roadmap.md)
- [Known gaps against the ACI spec](reviews/aci-spec-conformance-gaps.md)
- [System One endpoint design proposal](systemone-design.md)
- [Router-mode provider review process](reviews/router-mode-provider-review.md)
- [Router-mode soundness review](reviews/router-mode-soundness.md)
- [Router-mode load-balancing and cache review](reviews/router-mode-load-balancing-cache.md)
- each provider's dated `review.md`, linked from the
  [provider verification index](providers/README.md)

When a review record and the current implementation disagree, the source files
named in the living reference win.

## Documentation conventions

- `spec/aci.md` is normative for the protocol. Implementation docs describe the
  behavior of this repository.
- Provider `verification.md` files track the current adapter. Provider
  `review.md` files are dated admission records. Before citing a dated review
  externally, perform a currency check if the review is more than three months
  old or covers a release older than the one the provider currently deploys.
- Examples use the canonical `/v1/aci/*` artifact endpoints. The
  `/v1/attestation/report` and `/v1/signature/{id}` routes exist for legacy
  dstack-vllm-proxy clients.
- Security statements identify the policy or request constraint that makes the
  gateway fail closed. A provider name alone does not imply enforcement.
- Each fact has one home page. Other pages link to it instead of restating it.
- Every guide and reference has a row in this index. Notes about one fixture,
  vendored package, or asset set live beside it and are linked from the page
  that discusses that code.
- `python3 scripts/check_docs.py` fails when a Markdown file cannot be reached
  from the root README, or a relative link or heading anchor does not resolve.
