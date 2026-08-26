# ACI client integration plan

## Goal

Give Node applications one verified ACI connection primitive, then keep Pi and
other agent frameworks as thin transport adapters. Attestation verification,
measured-release appraisal, hostname validation and TLS SPKI pinning must have one
implementation.

## Current design

`@phala/aci-verifier/node` exposes `connectAci()`. A connection owns:

- the verified workload identity and its expiry,
- an exact-origin, hostname-validated, SPKI-pinned `fetch`,
- atomic `refresh()` and idempotent `close()` lifecycle methods.

Pi injects that fetch through its supported `StreamOptions.fetch` hook. It does
not replace `globalThis.fetch`. Each `createProvider()` also owns an immutable
brand profile, so Redpill, Phala Cloud and neutral ACI providers can coexist in
one process. Verified transport is an invariant of the ACI provider; there is
no configuration that downgrades it to ordinary CA-TLS.

Other Node SDKs should inject the same fetch instead of receiving dedicated ACI
packages. Documented examples currently cover OpenAI Node, OpenAI Agents JS,
Vercel AI SDK and LangChain JS. Software without a custom HTTP transport hook
uses `aci serve`; coding-agent CLI compatibility is documented in
[`../coding-agents.md`](../coding-agents.md).

## Release status and next steps

1. **npm release boundary: complete in the repository.** All four packages are
   coordinated at `0.2.0`, publish compiled ESM plus declarations, use normal
   semver dependencies, pass package/type lint, and install together from
   tarballs in a clean project. The OIDC workflow publishes verifier, core,
   Redpill, then Phala Cloud from a `clients-v<version>` GitHub Release.
2. **Publish reviewed deployment identities.** The Pi core accepts reviewed
   compose hashes from profile, config, or environment and passes them to
   `connectAci({ policy })`. Redpill and Phala release pipelines still need to
   publish their reviewed hashes and inject them into branded profiles; do not
   derive them from a live endpoint.
3. **Exercise live consumers.** The package and scoped-transport tests are in
   place. Before promoting the release, run Pi and both transport paths against
   the same live reviewed deployment and archive the accepted/rejected
   transcripts.
4. **Keep browser and non-HTTP boundaries explicit.** Browser clients cannot
   observe TLS SPKI, and the Node fetch transport does not secure WebSockets,
   MCP, tools or tracing. Continue using E2EE or `aci serve` for those paths.

## Release acceptance

- A clean project can install the published verifier and Pi packages without
  repository-relative dependencies.
- Two branded Pi providers can run in one process without sharing profile,
  config or connection state.
- Invalid quote, compose policy, hostname, SPKI, origin or identity expiry fails
  closed before model request bytes are sent.
- OpenAI Node, OpenAI Agents JS, Vercel AI SDK and LangChain JS examples use the
  same `connectAci()` API.
- publint and Are The Types Wrong accept every packed ESM/type surface.
- A clean project imports all four tarballs without TypeScript runtime loaders.
